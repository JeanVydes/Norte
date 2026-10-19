use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::id::ToolName;
use std::future::Future;

use crate::tool::{
    AnyFunction, FnDeclarator, FnExecutor, FunctionDeclaration, ToolArgs, ToolContext,
    ToolDeclaration, ToolDeclarator, ToolOutput, ToolSpec,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug as DebugBound;

struct Registered {
    function: Arc<dyn AnyFunction>,
    spec: ToolSpec,
}

/// Immutable name-to-tool map used by the engine and adapters.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<ToolName, Registered>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a registry from erased functions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for a duplicate name or an invalid spec.
    pub fn with_tools(tools: impl IntoIterator<Item = Arc<dyn AnyFunction>>) -> Result<Self> {
        let mut registry = Self::new();
        for tool in tools {
            registry.register(tool)?;
        }
        Ok(registry)
    }

    /// Adds one tool under its own name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the name is already registered.
    pub fn register(&mut self, tool: Arc<dyn AnyFunction>) -> Result<()> {
        let spec = tool.spec()?;
        if self.tools.contains_key(&spec.name) {
            return Err(Error::InvalidInput(format!(
                "duplicate tool: {}",
                spec.name
            )));
        }
        self.tools.insert(
            spec.name.clone(),
            Registered {
                function: tool,
                spec,
            },
        );
        Ok(())
    }

    /// Registers a typed function declaration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the name is already registered.
    pub fn register_declaration<A, R>(
        &mut self,
        declaration: FunctionDeclaration<A, R>,
    ) -> Result<()>
    where
        A: DeserializeOwned + DebugBound + ToolArgs + 'static,
        R: Serialize + Send + Sync + 'static,
    {
        self.register(Arc::new(declaration))
    }

    /// Registers a typed rich declaration that returns [`ToolOutput`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the name is already registered.
    pub fn register_tool_declaration<A>(&mut self, declaration: ToolDeclaration<A>) -> Result<()>
    where
        A: DeserializeOwned + DebugBound + ToolArgs + 'static,
    {
        self.register(Arc::new(declaration))
    }

    /// Declares and registers a plain typed declarator in one step. A tool whose
    /// executor returns [`ToolOutput`] uses [`register_tool`](Self::register_tool)
    /// instead, so the rich envelope is not wrapped a second time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the name is already registered.
    pub fn register_declarator<A, R, D>(&mut self, declarator: &D) -> Result<()>
    where
        A: DeserializeOwned + DebugBound + ToolArgs + 'static,
        R: Serialize + Send + Sync + 'static,
        D: FnDeclarator<A, R>,
    {
        self.register_declaration(declarator.declare())
    }

    /// Declares and registers a rich declarator, an executor returning
    /// [`ToolOutput`], in one step.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the name is already registered.
    pub fn register_tool<A, D>(&mut self, declarator: &D) -> Result<()>
    where
        A: DeserializeOwned + DebugBound + ToolArgs + 'static,
        D: ToolDeclarator<A>,
    {
        self.register_tool_declaration(declarator.declare_tool())
    }

    /// The function registered under `name`, if any.
    #[must_use]
    pub fn get(&self, name: &ToolName) -> Option<&Arc<dyn AnyFunction>> {
        self.tools.get(name).map(|registered| &registered.function)
    }

    /// The spec registered under `name`, if any.
    #[must_use]
    pub fn spec(&self, name: &ToolName) -> Option<&ToolSpec> {
        self.tools.get(name).map(|registered| &registered.spec)
    }

    /// Whether no tool is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// How many tools are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// The spec of every registered tool, ordered by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|registered| registered.spec.clone())
            .collect()
    }

    /// Runs one registered tool by name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ToolNotFound`] when the name is unknown, otherwise the
    /// tool's own error.
    pub async fn execute(
        &self,
        context: &ToolContext,
        name: &ToolName,
        arguments: Value,
    ) -> Result<ToolOutput> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| Error::ToolNotFound(name.clone()))?;
        tool.function.execute(context, &arguments).await
    }
}

impl Debug for ToolRegistry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Executor adapter so a plain async closure can back a tool without a struct.
pub struct FnTool<A, R, F> {
    function: F,
    _marker: std::marker::PhantomData<fn(A) -> R>,
}

impl<A, R, F> FnTool<A, R, F> {
    /// A tool whose executor is `function`.
    #[must_use]
    pub fn new(function: F) -> Self {
        Self {
            function,
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<A, R, F, Fut> FnExecutor<A, R> for FnTool<A, R, F>
where
    A: ToolArgs,
    R: Serialize + Send + Sync + 'static,
    F: Fn(A) -> Fut + Send + Sync,
    Fut: Future<Output = Result<R>> + Send,
{
    async fn call(&self, args: A) -> Result<R> {
        (self.function)(args).await
    }
}
