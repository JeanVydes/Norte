#![forbid(unsafe_code)]
#![doc = "`Norte` core: the domain model, the streaming pipeline, capabilities, and the contracts a provider or an adapter implements."]

extern crate self as norte_core;

/// Tool-call approval: records, grants and the audit trail.
pub mod approval;
/// The blob port: content-addressed bytes for media.
pub mod blob;
/// What a model declares it can do, and the typed features the engine checks.
pub mod capability;
/// Canonical byte encodings shared by persistence and the wire.
pub mod codec;
/// The value types a conversation is made of.
pub mod domain;
/// The error type and its variants.
pub mod error;
/// Events the engine emits while a turn runs.
pub mod event;
/// Typed identifiers, digests and wire families.
pub mod id;
/// The layer trait that wraps one model call.
pub mod layer;
/// The provider-file seam for media parts.
pub mod media_adapter;
/// The model port and the request it receives.
pub mod model;
/// Observers and stream transforms that watch a turn.
pub mod observer;
/// Media preprocessing before a request is built.
pub mod preprocess;
/// Stream events and the accumulator that folds them into parts.
pub mod stream;
/// Tool declarations, the registry and execution.
pub mod tool;

pub use approval::{Grant, GrantScope};
pub use blob::BlobStore;
pub use capability::{
    CacheMode, Capabilities, CapabilitiesBuilder, Feature, Modality, ModalityCapabilities,
    ModalityLimits, Reasoning, ReasoningEffort, ReasoningRequest, StreamingGranularity,
    StructuredOutputMode, ToolKind,
};
pub use domain::commit::Commit;
pub use domain::continuity::{
    ContinuityFragment, ContinuityScope, ContinuitySemantics, ContinuityToken, ReplayDecision,
};
pub use domain::conversation::{CONVERSATION_SCHEMA_VERSION, Conversation};
pub use domain::logprob::{LogprobAlternative, LogprobValue, TokenLogprob};
pub use domain::media::{
    MediaKind, MediaMetadata, MediaPart, MediaSource, MimeType, ProviderFileRef,
};
pub use domain::message::{
    CacheClass, FinishReason, Message, MessageOrigin, MessageStatus, Usage, VendorUsage,
};
pub use domain::moderation::SafetyRating;
pub use domain::node::{Node, NodeKind};
pub use domain::part::{
    Citation, CitationSource, ExtensionPart, Part, ProviderTool, ProviderToolCallPart,
    ProviderToolResultPart, ReasoningPart, ReasoningVisibility, RefusalPart, TextPart,
    ToolCallPart, ToolResultPart,
};
pub use domain::role::Role;
pub use domain::session::{Phase, Session};
pub use error::{Error, ProviderError, ProviderErrorKind, Result};
pub use event::{EngineEvent, EventSink, StreamDelta};
pub use id::{
    ApprovalId, BlobId, CallId, Digest, GenerationId, GrantId, ModelId, ModelRef, NodeId,
    ProviderId, SessionId, ToolName, WireFamily,
};
pub use layer::{
    Continuation, Layer, LayerContext, LayerStack, ModelContinuation, ShortCircuitReason,
    synthetic_stream,
};
pub use media_adapter::{MediaAdapter, ProviderFiles};
pub use model::{
    ContinuityAdapter, GenerationParams, MAX_TOP_LOGPROBS, Model, ModelRequest, ModelStream,
    PARTIAL_TOOLS_KEY, SESSION_EXTENSION, StreamLimits, TURN_OPTIONS_KEY, TurnOptions,
    splice_vendor_extensions,
};
pub use norte_macros::tool;
pub use observer::{Observer, ObserverSet, StreamTransform};
pub use preprocess::{MediaPayload, MediaPreprocessor};
pub use stream::{Accumulated, StreamAccumulator, StreamEvent};
pub use tool::registry::{FnTool, ToolRegistry};
pub use tool::{
    AnyFunction, Cancellation, FnDeclarator, FnExecutor, FunctionDeclaration, ProgressSink,
    ToolArgs, ToolCallInfo, ToolContext, ToolDeclaration, ToolDeclarator, ToolMetadata,
    ToolMetadataProvider, ToolOutput, ToolProgress, ToolSpec, ToolStream,
};
