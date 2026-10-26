/// Where declarations are collected and looked up by name.
pub mod registry;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use schemars::{JsonSchema, Schema};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::capability::ToolKind;
use crate::domain::media::MediaPart;
use crate::domain::part::{Part, TextPart};
use crate::error::{Error, Result};
use crate::id::{CallId, ModelRef, SessionId, ToolName};

/// A cooperative cancellation signal, owned by the core (NFR-4).
///
/// The reference implementation wraps [`tokio_util::sync::CancellationToken`],
/// but the boundary type is ours, so the tool trait surface stays async-runtime
/// agnostic.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(CancellationToken);

impl Cancellation {
    /// A token that starts uncancelled.
    #[must_use]
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    /// Signals every waiter bound to this token.
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// Whether cancellation has been signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Resolves when cancellation is signalled (or immediately if it already is).
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

impl From<CancellationToken> for Cancellation {
    fn from(value: CancellationToken) -> Self {
        Self(value)
    }
}

/// A sink for best-effort tool progress (FR-TOOL-5, NFR-4).
///
/// Runtime-agnostic on purpose: the reference implementation provides a blanket
/// impl for [`tokio::sync::mpsc::Sender`].
pub trait ProgressSink: Send + Sync {
    /// Delivers one progress chunk; a slow or gone consumer may drop it.
    fn send(&self, progress: ToolProgress);
}

impl ProgressSink for mpsc::Sender<ToolProgress> {
    fn send(&self, progress: ToolProgress) {
        if self.try_send(progress).is_err() {
            tracing::debug!("tool progress dropped: consumer full or closed");
        }
    }
}

/// Ambient data available to a tool while it runs: identity, cancellation and a
/// bounded progress channel.
///
/// It reaches tools through [`FnExecutor::call_with_context`], whose default
/// implementation ignores it, so a tool written against the simpler `call`
/// signature keeps compiling unchanged.
#[derive(Clone)]
pub struct ToolContext {
    /// The session the call belongs to, when the engine runs it.
    pub session: Option<SessionId>,
    /// The call.
    pub call_id: Option<CallId>,
    /// Free-form data the host attached to the call.
    pub metadata: BTreeMap<String, Value>,
    cancellation: Cancellation,
    /// Asked to stop waiting without killing what it runs.
    ///
    /// It is not `cancellation`: a host that detaches a call wants the process
    /// alive and its output still coming, which is the opposite of what
    /// cancelling asks for. A tool that can honour it returns early; one that
    /// cannot keeps running until its own bound, and the host is told so.
    detach: Cancellation,
    progress: Option<Arc<dyn ProgressSink>>,
}

impl ToolContext {
    /// A context with no session, call, metadata or progress sink.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: None,
            call_id: None,
            metadata: BTreeMap::new(),
            cancellation: Cancellation::new(),
            detach: Cancellation::new(),
            progress: None,
        }
    }

    /// Sets the session the call belongs to.
    #[must_use]
    pub fn with_session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// Sets the call.
    #[must_use]
    pub fn with_call(mut self, call_id: CallId) -> Self {
        self.call_id = Some(call_id);
        self
    }

    /// Binds the context to a cancellation token.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: impl Into<Cancellation>) -> Self {
        self.cancellation = cancellation.into();
        self
    }

    /// Delivers progress to the given sink.
    #[must_use]
    pub fn with_progress(mut self, progress: Arc<dyn ProgressSink>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Keeps one more host value under its own key.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// The cancellation token the turn shares with the tool.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// Resolves when the turn cancels (or immediately if it already did).
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    /// Whether the turn was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Sets the token a host signals to detach the call.
    #[must_use]
    pub fn with_detach(mut self, detach: Cancellation) -> Self {
        self.detach = detach;
        self
    }

    /// The detach token, so a host can signal it from outside the call.
    #[must_use]
    pub fn detach(&self) -> Cancellation {
        self.detach.clone()
    }

    /// Resolves when the call is asked to stop waiting (or at once if it was).
    pub async fn detached(&self) {
        self.detach.cancelled().await;
    }

    /// Whether the call was asked to stop waiting.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        self.detach.is_cancelled()
    }

    /// The sink progress goes to, so a wrapper can pass it on.
    ///
    /// A host that wraps a call to watch it substitutes a sink of its own; this
    /// is what lets that sink keep delivering to the engine's while it decides
    /// not to, and stop when the call is no longer the engine's to report.
    #[must_use]
    pub fn progress(&self) -> Option<Arc<dyn ProgressSink>> {
        self.progress.clone()
    }

    /// Best-effort progress. It is not a durability event: if the consumer is
    /// slow or gone, the update is dropped.
    pub fn emit(&self, progress: ToolProgress) {
        if let Some(sink) = &self.progress {
            sink.send(progress);
        }
    }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("session", &self.session)
            .field("call_id", &self.call_id)
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// What the engine tells the host's metadata provider about one call.
///
/// The fields are the call the executor is about to receive: the session and
/// turn it belongs to, the call id, the tool and the arguments after any policy
/// transform. The provider is consulted immediately before execution, so a call
/// that resumes after an approval park is filled exactly like one that ran
/// inline (FR-TOOL-7).
#[derive(Clone, Debug)]
pub struct ToolCallInfo {
    /// The session the call belongs to.
    pub session: SessionId,
    /// The turn the call belongs to.
    pub turn: u64,
    /// The call.
    pub call_id: CallId,
    /// The tool the call names.
    pub tool: ToolName,
    /// The arguments, after any policy transform.
    pub arguments: Value,
}

/// The host callback behind a [`ToolMetadataProvider`].
type MetadataFn = dyn Fn(&ToolCallInfo) -> BTreeMap<String, Value> + Send + Sync;

/// Supplies [`ToolContext::metadata`] for each execution.
///
/// The engine calls it right before the executor runs, both on the first
/// attempt and when a parked call resumes, so host data (tenant, actor, trace
/// id, a short-lived credential) reaches the tool without being written to the
/// session, the graph or the wire (FR-TOOL-7).
#[derive(Clone)]
pub struct ToolMetadataProvider(Arc<MetadataFn>);

impl ToolMetadataProvider {
    /// Wraps a host callback.
    #[must_use]
    pub fn new(
        provider: impl Fn(&ToolCallInfo) -> BTreeMap<String, Value> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(provider))
    }

    /// The metadata for one call.
    #[must_use]
    pub fn metadata(&self, info: &ToolCallInfo) -> BTreeMap<String, Value> {
        (self.0)(info)
    }
}

impl Debug for ToolMetadataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolMetadataProvider(..)")
    }
}

/// Which output stream a progress chunk came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// One progress chunk from a running tool.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolProgress {
    /// The stream the bytes came from.
    pub stream: ToolStream,
    /// The bytes themselves.
    #[serde(with = "crate::codec::bytes_base64")]
    pub bytes: Vec<u8>,
    /// Whether the chunk ends that stream.
    pub last: bool,
}

/// What a tool gives back. The engine packs it into the single Tool message the
/// next model turn sees.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    /// The result's content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The result's structured value, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// Whether the tool reported an error.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolOutput {
    /// A result carrying one text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![Part::Text(TextPart::new(text))],
            structured: None,
            is_error: false,
        }
    }

    /// A result carrying a structured value.
    #[must_use]
    pub fn json(value: Value) -> Self {
        Self {
            parts: Vec::new(),
            structured: Some(value),
            is_error: false,
        }
    }

    /// An error result carrying one text part.
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }

    /// Adds a media part to the result.
    #[must_use]
    pub fn with_media(mut self, media: MediaPart) -> Self {
        self.parts.push(Part::Media(media));
        self
    }
}

/// A typed tool argument type: JSON-schema describable, serializable and usable
/// as a call payload.
pub trait ToolArgs: JsonSchema + Serialize + Send + Sync + 'static {}

impl<T> ToolArgs for T where T: JsonSchema + Serialize + Send + Sync + 'static {}

/// The original `Norte` tool shape: a typed call from `A` to `R`.
#[async_trait::async_trait]
pub trait FnExecutor<A, R>: Send + Sync
where
    A: ToolArgs,
    R: Serialize + Send + Sync + 'static,
{
    /// Runs the tool with parsed arguments.
    async fn call(&self, args: A) -> Result<R>;

    /// Context-aware entry point. The default keeps existing tools untouched.
    async fn call_with_context(&self, _context: &ToolContext, args: A) -> Result<R> {
        self.call(args).await
    }
}

/// Static name and description for a tool type.
///
/// The `#[tool(...)]` attribute macro implements this trait for a struct, but it
/// can also be written by hand. Two registrations build on it:
///
/// - [`FnDeclarator`] for plain tools, whose executor returns a serializable
///   result;
/// - [`ToolDeclarator`] for rich tools, whose executor returns [`ToolOutput`].
///
/// Both are implemented for any type that combines [`FnExecutor`] with this trait
/// and `Clone`, so a registration never restates the argument and result types.
pub trait ToolMetadata {
    /// Registered tool name, as sent to the model.
    const NAME: &'static str;

    /// Human-readable description shown to the model.
    const DESCRIPTION: &'static str;
}

/// Declares a plain tool from its metadata and executor.
pub trait FnDeclarator<A, R>: Send + Sync
where
    A: ToolArgs,
    R: Serialize + Send + Sync + 'static,
{
    /// The declaration: name, description and the schema generated from `A`.
    fn declare(&self) -> FunctionDeclaration<A, R>;
}

impl<A, R, T> FnDeclarator<A, R> for T
where
    A: ToolArgs + DeserializeOwned + Debug + 'static,
    R: Serialize + Send + Sync + 'static,
    T: FnExecutor<A, R> + ToolMetadata + Clone + Send + Sync + 'static,
{
    fn declare(&self) -> FunctionDeclaration<A, R> {
        FunctionDeclaration::new(T::NAME, T::DESCRIPTION, Arc::new(self.clone()))
    }
}

/// A typed rich-tool registration: like [`FnDeclarator`], but for a tool whose
/// executor returns the [`ToolOutput`] envelope directly, so text parts, media
/// and the error flag reach the model untouched.
pub trait ToolDeclarator<A>: Send + Sync
where
    A: ToolArgs,
{
    /// The declaration: name, description and the schema generated from `A`.
    fn declare_tool(&self) -> ToolDeclaration<A>;
}

impl<A, T> ToolDeclarator<A> for T
where
    A: ToolArgs + DeserializeOwned + Debug + 'static,
    T: FnExecutor<A, ToolOutput> + ToolMetadata + Clone + Send + Sync + 'static,
{
    fn declare_tool(&self) -> ToolDeclaration<A> {
        ToolDeclaration::new(T::NAME, T::DESCRIPTION, Arc::new(self.clone()))
    }
}

/// Expands a schemars document into a self-contained one.
///
/// Every local `$ref` is replaced by the definition it points at and the
/// definition block is dropped, so a model that does not resolve references
/// still sees the whole shape — the failure mode is a model sending a string
/// where the object belongs. Keywords sitting next to a `$ref` (a `default`,
/// say) are kept.
///
/// A reference that cannot be inlined — an unknown name, or a type that is
/// recursive and therefore infinite — is left alone, and then the definition
/// block stays so the document remains valid.
fn inline_schema(schema: &Schema) -> Result<Value> {
    let value = serde_json::to_value(schema)?;
    let mut expanded = expand_refs(&value, &value, &mut Vec::new());
    if !has_ref(&expanded) {
        if let Some(object) = expanded.as_object_mut() {
            object.remove("$defs");
            object.remove("definitions");
        }
    }
    Ok(expanded)
}

/// Recursively replaces a local reference with the definition it points at,
/// keeping any sibling keywords. `open` holds the references currently being
/// expanded, which is how a recursive type terminates instead of looping
/// forever.
fn expand_refs(value: &Value, root: &Value, open: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if !open.iter().any(|entry| entry == reference) {
                    if let Some(target) = lookup(root, reference) {
                        open.push(reference.to_owned());
                        let mut resolved = expand_refs(target, root, open);
                        open.pop();
                        if let Some(fields) = resolved.as_object_mut() {
                            for (key, sibling) in object {
                                if key != "$ref" {
                                    fields.insert(key.clone(), expand_refs(sibling, root, open));
                                }
                            }
                        }
                        return resolved;
                    }
                }
            }
            Value::Object(
                object
                    .iter()
                    .map(|(key, item)| (key.clone(), expand_refs(item, root, open)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| expand_refs(item, root, open))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Resolves a local reference such as `#/$defs/CommandLine` against the
/// document. Both spellings of the definition block are honoured: `$defs`, what
/// schemars emits, and `definitions`, what a draft-07 document such as an MCP
/// server's input schema may publish. Anything else (a remote reference, `#`
/// itself) resolves to nothing and is left alone.
fn lookup<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let (block, name) = reference
        .strip_prefix("#/$defs/")
        .map(|name| ("$defs", name))
        .or_else(|| {
            reference
                .strip_prefix("#/definitions/")
                .map(|name| ("definitions", name))
        })?;
    root.get(block)?.get(name)
}

/// Whether any `$ref` survived expansion.
fn has_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.contains_key("$ref") || object.values().any(has_ref),
        Value::Array(items) => items.iter().any(has_ref),
        _ => false,
    }
}

/// Everything the engine and adapters need to know about one tool.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolSpec {
    /// The tool's name.
    pub name: ToolName,
    /// The text the model reads.
    pub description: String,
    /// How the tool reaches the model.
    pub kind: ToolKind,
    /// The argument schema, self-contained: local references are already
    /// resolved, so a model that does not follow `$ref` sees the whole shape.
    pub parameters: Value,
    /// The provider-side declaration of a tool the host does not execute (a
    /// built-in such as `web_search`), re-emitted verbatim on the wire (Section
    /// 12). [`Option::None`] for a host-executed function tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<Value>,
    /// The approval prompt the host attaches to the call, where it attaches one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
}

impl ToolSpec {
    /// The provider declaration to re-emit on the wire, or a typed capability
    /// error when the host did not supply one (Section 12: never a silent
    /// omission).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capability`] when `declaration` is [`Option::None`].
    pub fn provider_declaration(&self, model: &ModelRef, wire: &str) -> Result<&Value> {
        self.declaration.as_ref().ok_or_else(|| Error::Capability {
            model: model.clone(),
            feature: format!(
                "tool `{}` ({:?}) has no provider declaration on the {wire} wire",
                self.name.as_str(),
                self.kind
            ),
        })
    }
}

impl ToolSpec {
    /// Strict argument parsing, used by hand-written [`AnyFunction`] impls.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidToolArguments`] when the value does not match the
    /// declared argument type.
    pub fn parse<A: DeserializeOwned>(&self, arguments: &Value) -> Result<A> {
        serde_json::from_value(arguments.clone()).map_err(|error| Error::InvalidToolArguments {
            tool: self.name.clone(),
            reason: error.to_string(),
        })
    }
}

/// A typed tool registration: name, description, generated schema and the
/// executor that runs it.
#[derive(Clone)]
pub struct FunctionDeclaration<A, R>
where
    A: ToolArgs,
    R: Serialize + Send + Sync,
{
    /// The name the model calls.
    pub name: &'static str,
    /// The text the model reads.
    pub description: &'static str,
    /// The schema generated from the argument type.
    pub parameters: Schema,
    /// The code that runs.
    pub executor: Arc<dyn FnExecutor<A, R>>,
    /// The approval prompt the host attaches to a call, where it attaches one.
    pub approval: Option<String>,
}

impl<A, R> FunctionDeclaration<A, R>
where
    A: ToolArgs + DeserializeOwned + Debug + 'static,
    R: Serialize + Send + Sync + 'static,
{
    /// A declaration for `name`, described by `description`, running
    /// `executor`.
    #[must_use]
    pub fn new(
        name: &'static str,
        description: &'static str,
        executor: Arc<dyn FnExecutor<A, R>>,
    ) -> Self {
        Self {
            name,
            description,
            parameters: schemars::schema_for!(A),
            executor,
            approval: None,
        }
    }

    /// Attaches an approval prompt to the tool.
    #[must_use]
    pub fn with_approval(mut self, approval: impl Into<String>) -> Self {
        self.approval = Some(approval.into());
        self
    }
}

impl<A, R> Debug for FunctionDeclaration<A, R>
where
    A: ToolArgs,
    R: Serialize + Send + Sync,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionDeclaration")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("approval", &self.approval)
            .finish_non_exhaustive()
    }
}

/// A typed tool that returns the rich [`ToolOutput`] envelope directly, for
/// tools that need media parts, an error flag or truncation metadata.
///
/// Ordinary tools use [`FunctionDeclaration`]; this is the same typed shape with
/// a richer result.
#[derive(Clone)]
pub struct ToolDeclaration<A>
where
    A: ToolArgs,
{
    /// The name the model calls.
    pub name: &'static str,
    /// The text the model reads.
    pub description: &'static str,
    /// The schema generated from the argument type.
    pub parameters: Schema,
    /// The code that runs.
    pub executor: Arc<dyn FnExecutor<A, ToolOutput>>,
    /// The approval prompt the host attaches to a call, where it attaches one.
    pub approval: Option<String>,
}

impl<A> ToolDeclaration<A>
where
    A: ToolArgs + DeserializeOwned + Debug + 'static,
{
    /// A declaration for `name`, described by `description`, running
    /// `executor`.
    #[must_use]
    pub fn new(
        name: &'static str,
        description: &'static str,
        executor: Arc<dyn FnExecutor<A, ToolOutput>>,
    ) -> Self {
        Self {
            name,
            description,
            parameters: schemars::schema_for!(A),
            executor,
            approval: None,
        }
    }

    /// Attaches an approval prompt to the tool.
    #[must_use]
    pub fn with_approval(mut self, approval: impl Into<String>) -> Self {
        self.approval = Some(approval.into());
        self
    }
}

impl<A> Debug for ToolDeclaration<A>
where
    A: ToolArgs,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDeclaration")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("approval", &self.approval)
            .finish_non_exhaustive()
    }
}

/// The erased view the registry and the engine use. Tool authors normally never
/// implement this: a [`FunctionDeclaration`] or [`ToolDeclaration`] does.
#[async_trait::async_trait]
pub trait AnyFunction: Send + Sync + Debug {
    /// The name the model calls.
    fn name(&self) -> &str;

    /// The text the model reads.
    fn description(&self) -> &str;

    /// The schema generated from the argument type.
    fn parameters_schema(&self) -> &Schema;

    /// How the tool reaches the model.
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    /// The approval prompt the host attaches to a call, where it attaches one.
    fn approval(&self) -> Option<&str> {
        None
    }

    /// The provider-side declaration of a tool this host does not execute (a
    /// built-in such as `web_search`), re-emitted verbatim on the wire (Section
    /// 12). [`Option::None`] for a host-executed function tool.
    fn declaration(&self) -> Option<Value> {
        None
    }

    /// Runs the tool against raw JSON arguments.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidToolArguments`] on bad arguments and whatever the
    /// tool body returns.
    async fn execute(&self, context: &ToolContext, arguments: &Value) -> Result<ToolOutput>;

    /// Derives the registry-facing spec from the declaration.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool name is invalid or the schema cannot be
    /// serialized.
    fn spec(&self) -> Result<ToolSpec> {
        Ok(ToolSpec {
            name: ToolName::new(self.name())?,
            description: self.description().to_owned(),
            kind: self.kind(),
            parameters: inline_schema(self.parameters_schema())?,
            declaration: self.declaration(),
            approval: self.approval().map(str::to_owned),
        })
    }
}

#[async_trait::async_trait]
impl<A, R> AnyFunction for FunctionDeclaration<A, R>
where
    A: DeserializeOwned + Debug + ToolArgs + 'static,
    R: Serialize + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> &Schema {
        &self.parameters
    }

    fn approval(&self) -> Option<&str> {
        self.approval.as_deref()
    }

    async fn execute(&self, context: &ToolContext, arguments: &Value) -> Result<ToolOutput> {
        let tool = ToolName::new(self.name)?;
        let concrete: A = serde_json::from_value(arguments.clone()).map_err(|error| {
            Error::InvalidToolArguments {
                tool: tool.clone(),
                reason: error.to_string(),
            }
        })?;
        let result = self.executor.call_with_context(context, concrete).await?;
        Ok(ToolOutput::json(serde_json::to_value(result)?))
    }
}

impl<A, R> From<FunctionDeclaration<A, R>> for Arc<dyn AnyFunction>
where
    A: DeserializeOwned + Debug + ToolArgs + 'static,
    R: Serialize + Send + Sync + 'static,
{
    fn from(declaration: FunctionDeclaration<A, R>) -> Self {
        Arc::new(declaration)
    }
}

#[async_trait::async_trait]
impl<A> AnyFunction for ToolDeclaration<A>
where
    A: DeserializeOwned + Debug + ToolArgs + 'static,
{
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> &Schema {
        &self.parameters
    }

    fn approval(&self) -> Option<&str> {
        self.approval.as_deref()
    }

    async fn execute(&self, context: &ToolContext, arguments: &Value) -> Result<ToolOutput> {
        let tool = ToolName::new(self.name)?;
        let concrete: A = serde_json::from_value(arguments.clone()).map_err(|error| {
            Error::InvalidToolArguments {
                tool: tool.clone(),
                reason: error.to_string(),
            }
        })?;
        self.executor.call_with_context(context, concrete).await
    }
}

impl<A> From<ToolDeclaration<A>> for Arc<dyn AnyFunction>
where
    A: DeserializeOwned + Debug + ToolArgs + 'static,
{
    fn from(declaration: ToolDeclaration<A>) -> Self {
        Arc::new(declaration)
    }
}

/// Declares a typed tool as a `FunctionDeclaration`.
#[macro_export]
macro_rules! declare_function {
    ($name:expr, $description:expr, $args:ty, $result:ty, $executor:expr) => {
        $crate::FunctionDeclaration::new($name, $description, ::std::sync::Arc::new($executor))
    };
}

/// Declares a rich tool as a `ToolDeclaration`.
#[macro_export]
macro_rules! declare_tool {
    ($name:expr, $description:expr, $args:ty, $executor:expr) => {
        $crate::ToolDeclaration::new($name, $description, ::std::sync::Arc::new($executor))
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::tool::registry::FnTool;
    use serde_json::json;

    #[derive(Debug, JsonSchema, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        path: String,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct Output {
        content: String,
    }

    #[norte_core::tool(name = "read_file", description = "Read a file")]
    #[derive(Debug)]
    struct ReadFile;

    #[async_trait::async_trait]
    impl FnExecutor<Args, Output> for ReadFile {
        async fn call(&self, args: Args) -> Result<Output> {
            Ok(Output { content: args.path })
        }
    }

    #[test]
    fn the_attribute_carries_the_metadata() {
        assert_eq!(ReadFile::NAME, "read_file");
        assert_eq!(ReadFile::DESCRIPTION, "Read a file");
    }

    #[derive(Debug, JsonSchema, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Mode {
        Fast { retries: u32 },
        Slow,
    }

    #[derive(Debug, JsonSchema, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NestedArgs {
        mode: Mode,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct NestedOutput {
        ok: bool,
    }

    #[norte_core::tool(name = "nested", description = "Nested arguments")]
    #[derive(Debug)]
    struct Nested;

    #[async_trait::async_trait]
    impl FnExecutor<NestedArgs, NestedOutput> for Nested {
        async fn call(&self, _args: NestedArgs) -> Result<NestedOutput> {
            Ok(NestedOutput { ok: true })
        }
    }

    #[test]
    fn the_wire_schema_is_self_contained() {
        // Schemars emits `$defs` plus `$ref` for a nested type. Models that do
        // not follow references then guess the shape and send nonsense, so the
        // declaration must reach the wire already expanded.
        let declaration: FunctionDeclaration<NestedArgs, NestedOutput> = Nested.declare();
        let function: Arc<dyn AnyFunction> = declaration.into();
        let spec = function.spec().expect("spec");
        let rendered = serde_json::to_string(&spec.parameters).expect("bytes");
        assert!(!rendered.contains("$ref"), "references must be inlined");
        assert!(!rendered.contains("$defs"), "definitions must be dropped");
        assert!(
            rendered.contains("\"retries\""),
            "the inlined definition must carry its content"
        );
    }

    #[test]
    fn a_draft_seven_definition_block_is_inlined_too() {
        let schema: Schema = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "line": { "$ref": "#/definitions/Line", "description": "the line" }
            },
            "required": ["line"],
            "definitions": {
                "Line": {
                    "type": "object",
                    "properties": { "program": { "type": "string" } },
                    "required": ["program"]
                }
            }
        }))
        .expect("schema");
        let rendered = inline_schema(&schema).expect("inline");
        let text = serde_json::to_string(&rendered).expect("bytes");
        assert!(!text.contains("$ref"), "references must be inlined: {text}");
        assert!(!text.contains("definitions"), "the block must be dropped");
        assert_eq!(
            rendered["properties"]["line"]["properties"]["program"]["type"],
            json!("string")
        );
        assert_eq!(
            rendered["properties"]["line"]["description"],
            json!("the line"),
            "a keyword next to the reference must survive"
        );
    }

    #[test]
    fn a_recursive_schema_stays_valid() {
        let schema: Schema = serde_json::from_value(json!({
            "type": "object",
            "properties": { "next": { "$ref": "#/$defs/Node" } },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/Node" } }
                }
            }
        }))
        .expect("schema");
        let rendered = inline_schema(&schema).expect("inline");
        assert!(has_ref(&rendered), "a cycle cannot be inlined");
        assert!(
            rendered.get("$defs").is_some(),
            "the block must stay when a reference survives"
        );
    }

    #[tokio::test]
    async fn cancellation_propagates_through_the_context() {
        let context = ToolContext::new();
        let signal = context.cancellation();
        assert!(!context.is_cancelled());
        signal.cancel();
        assert!(context.is_cancelled());
        context.cancelled().await;
    }

    #[test]
    fn progress_reaches_a_host_supplied_sink() {
        #[derive(Default)]
        struct Sink(Mutex<Vec<ToolProgress>>);

        impl ProgressSink for Sink {
            fn send(&self, progress: ToolProgress) {
                self.0.lock().expect("lock").push(progress);
            }
        }

        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        context.emit(ToolProgress {
            stream: ToolStream::Stderr,
            bytes: b"hi".to_vec(),
            last: false,
        });
        let sent = sink.0.lock().expect("lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].stream, ToolStream::Stderr);
    }

    #[test]
    fn a_wrapper_can_read_the_sink_and_pass_progress_on() {
        // What a host wrapper needs: the engine's sink, and an `emit` that still
        // reaches it. Without the getter the wrapper could only replace the sink,
        // so the live view would end at the wrapper's door.
        #[derive(Default)]
        struct Sink(Mutex<Vec<ToolProgress>>);

        impl ProgressSink for Sink {
            fn send(&self, progress: ToolProgress) {
                self.0.lock().expect("lock").push(progress);
            }
        }

        let engine = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(engine.clone());
        let passed_on = context.progress().expect("the sink the engine installed");
        passed_on.send(ToolProgress {
            stream: ToolStream::Stdout,
            bytes: b"forwarded".to_vec(),
            last: false,
        });

        let sent = engine.0.lock().expect("lock");
        assert_eq!(
            sent.len(),
            1,
            "the engine's sink received what was passed on"
        );
        assert_eq!(sent[0].bytes, b"forwarded");
    }

    #[tokio::test]
    async fn typed_tool_runs_through_the_erased_view() {
        let declaration: FunctionDeclaration<Args, Output> = ReadFile.declare();
        let function: Arc<dyn AnyFunction> = declaration.into();
        let output = function
            .execute(&ToolContext::new(), &serde_json::json!({ "path": "a" }))
            .await
            .expect("execute");
        assert_eq!(
            output.structured,
            Some(serde_json::json!({ "content": "a" }))
        );
    }

    #[tokio::test]
    async fn unknown_arguments_are_typed_errors() {
        let declaration: FunctionDeclaration<Args, Output> = ReadFile.declare();
        let function: Arc<dyn AnyFunction> = declaration.into();
        let error = function
            .execute(
                &ToolContext::new(),
                &serde_json::json!({ "path": "a", "extra": true }),
            )
            .await
            .expect_err("unknown field");
        assert!(matches!(error, Error::InvalidToolArguments { .. }));
    }

    #[test]
    fn specs_are_derived_from_the_declaration() {
        let declaration: FunctionDeclaration<Args, Output> = ReadFile.declare();
        let function: Arc<dyn AnyFunction> = declaration.into();
        let spec = function.spec().expect("spec");
        assert_eq!(spec.name.as_str(), "read_file");
        assert!(spec.parameters.get("properties").is_some());
    }

    #[tokio::test]
    async fn raw_executors_still_wrap_through_the_macro() {
        let declaration = declare_function!(
            "read_file",
            "Read a file",
            Args,
            Output,
            FnTool::<Args, Output, _>::new(|args: Args| async move {
                Ok(Output { content: args.path })
            })
        );
        let function: Arc<dyn AnyFunction> = declaration.into();
        let output = function
            .execute(&ToolContext::new(), &serde_json::json!({ "path": "b" }))
            .await
            .expect("execute");
        assert_eq!(
            output.structured,
            Some(serde_json::json!({ "content": "b" }))
        );
    }
}
