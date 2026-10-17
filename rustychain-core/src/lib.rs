#![forbid(unsafe_code)]
#![doc = "`RustyChain` core: domain model, engine, streaming pipeline, capabilities and contracts."]

extern crate self as rustychain_core;

pub mod approval;
pub mod capability;
pub mod codec;
pub mod domain;
pub mod engine;
pub mod error;
pub mod event;
pub mod id;
pub mod layer;
pub mod media_adapter;
pub mod model;
pub mod observer;
pub mod policy;
pub mod preprocess;
pub mod store;
pub mod stream;
pub mod tool;

pub use approval::{ApprovalRecord, ApprovalStatus, AuditAction, AuditEntry, Grant, GrantScope};
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
pub use domain::media::{
    MediaKind, MediaMetadata, MediaPart, MediaSource, MimeType, ProviderFileRef,
};
pub use domain::message::{CacheClass, FinishReason, Message, MessageOrigin, MessageStatus, Usage};
pub use domain::moderation::SafetyRating;
pub use domain::node::{Node, NodeKind};
pub use domain::part::{
    Citation, CitationSource, ExtensionPart, Part, ProviderTool, ProviderToolCallPart,
    ProviderToolResultPart, ReasoningPart, ReasoningVisibility, RefusalPart, TextPart,
    ToolCallPart, ToolResultPart,
};
pub use domain::role::Role;
pub use domain::session::{Phase, Session};
pub use engine::{
    Engine, EngineBuilder, EngineConfig, EngineStream, TurnBuilder, TurnOutcome, usage_of,
};
pub use error::{Error, ProviderError, ProviderErrorKind, Result};
pub use event::{EngineEvent, EventSink};
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
    ContinuityAdapter, GenerationParams, Model, ModelRequest, ModelStream, SESSION_EXTENSION,
    StreamLimits, TURN_OPTIONS_KEY, TurnOptions, splice_vendor_extensions,
};
pub use observer::{Observer, ObserverSet, StreamTransform};
pub use policy::{
    Decision, Policy, PolicyDecision, PolicyOutcome, PolicySet, PolicyView, ToolApprovalPolicy,
    combine,
};
pub use preprocess::{MediaPayload, MediaPreprocessor};
pub use rustychain_macros::tool;
pub use store::{BlobStore, Store};
pub use stream::{Accumulated, StreamAccumulator, StreamEvent};
pub use tool::registry::{FnTool, ToolRegistry};
pub use tool::{
    AnyFunction, Cancellation, FnDeclarator, FnExecutor, FunctionDeclaration, ProgressSink,
    ToolArgs, ToolContext, ToolDeclaration, ToolDeclarator, ToolMetadata, ToolOutput, ToolProgress,
    ToolSpec, ToolStream,
};
