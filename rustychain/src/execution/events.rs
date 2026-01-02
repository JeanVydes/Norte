use crate::agent::status::AgentStatus;
use crate::context::strategy::ContextManagementStrategy;
use crate::execution::node::{NodeId, NodeStatus, NodeType};
use crate::inference::UsageMetadata;
use crate::{FunctionCall, FunctionResult, GenerationConfig, Inference};
use std::time::Duration;

/// Events emitted during agent execution
///
/// Events are categorized into:
/// - Observable: Notification-only, non-blocking
/// - Interceptable: Can block execution and modify behavior
#[derive(Debug, Clone)]
pub enum Event<'a> {
    // ============ AGENT LIFECYCLE EVENTS (Observable) ============
    /// Agent has been initialized with configuration
    AgentInitialized {
        name: String,
        max_iterations: usize,
        has_context_management: bool,
    },

    /// Agent status has changed
    AgentStatusChanged { from: AgentStatus, to: AgentStatus },

    /// Agent has been reset (soft or hard)
    AgentReset {
        hard: bool,
        previous_root: Option<NodeId>,
    },

    /// Agent is approaching iteration limit
    MaxIterationsWarning {
        current: usize,
        max: usize,
        remaining: usize,
    },

    // ============ TURN/STEP EVENTS (Observable) ============
    /// New turn started with user input
    TurnStarted {
        turn_number: usize,
        inference: &'a Inference,
    },

    /// Turn completed successfully
    TurnCompleted {
        turn_number: usize,
        total_iterations: usize,
        final_node: NodeId,
    },

    /// Single iteration step started
    IterationStarted {
        iteration: usize,
        current_node: NodeId,
    },

    /// Single iteration step completed
    IterationCompleted {
        iteration: usize,
        node_id: NodeId,
        duration: Duration,
    },

    /// Execution step completed (legacy, kept for compatibility)
    StepCompleted { step: usize, node_id: NodeId },

    // ============ NODE EVENTS (Observable) ============
    /// New execution node created
    NodeCreated {
        node_id: NodeId,
        node_type: NodeType,
        parent: Option<NodeId>,
    },

    /// Node status changed
    NodeStatusChanged {
        node_id: NodeId,
        from: NodeStatus,
        to: NodeStatus,
    },

    /// Node updated (generic update event)
    NodeUpdated {
        node_id: NodeId,
        update_type: NodeUpdateType,
    },

    /// Current node pointer changed
    CurrentNodeChanged { from: Option<NodeId>, to: NodeId },

    /// Conversation root updated
    ConversationRootUpdated {
        previous: Option<NodeId>,
        new_root: NodeId,
    },

    // ============ INFERENCE EVENTS (Observable & Interceptable) ============
    /// Input inference received (observable)
    InputInference { inference: &'a Inference },

    /// About to start processing inference (interceptable)
    InferenceAboutToStart {
        node_id: NodeId,
        inference: &'a Inference,
    },

    /// Inference processing started (observable)
    InferenceStarted {
        node_id: NodeId,
        inference: &'a Inference,
    },

    /// Inference result received from LLM (observable)
    InferenceResultReceived {
        node_id: NodeId,
        inference: &'a Inference,
    },

    /// Inference processing completed (observable)
    InferenceCompleted {
        node_id: NodeId,
        inference: &'a Inference,
        duration: Duration,
    },

    // ============ CONTEXT/HISTORY EVENTS (Observable & Interceptable) ============
    /// History built before optimization (interceptable)
    HistoryBuilt {
        node_id: NodeId,
        history: &'a Vec<Inference>,
        message_count: usize,
        estimated_tokens: usize,
    },

    /// About to apply context optimization (interceptable)
    ContextOptimizationStarted {
        original_length: usize,
        strategies: &'a [ContextManagementStrategy],
    },

    /// Individual context strategy applied (observable)
    ContextStrategyApplied {
        strategy_index: usize,
        strategy_name: String,
        before_length: usize,
        after_length: usize,
        duration: Duration,
    },

    /// Context optimization completed (observable)
    ContextOptimized {
        original_length: usize,
        optimized_length: usize,
        applied_strategies: Vec<ContextManagementStrategy>,
        total_duration: Duration,
    },

    // ============ LLM REQUEST EVENTS (Observable & Interceptable) ============
    /// About to send request to LLM (interceptable)
    LLMRequestAboutToSend {
        inference: &'a Inference,
        history: &'a Vec<Inference>,
        config: &'a GenerationConfig,
    },

    /// LLM request sent (observable)
    LLMRequestSent {
        message_count: usize,
        estimated_tokens: usize,
        model: String,
    },

    /// LLM request completed (observable)
    LLMRequestCompleted {
        result: &'a Inference,
        duration: Duration,
    },

    /// Inference Usage Metadata received (observable)
    InferenceUsageMetadata(Option<UsageMetadata>),

    // ============ TOOL EXECUTION EVENTS (Observable & Interceptable) ============
    /// Tool batch execution started (observable)
    ToolBatchStarted {
        node_id: NodeId,
        call_count: usize,
        tool_names: Vec<String>,
    },

    /// Individual tool execution started (observable)
    ToolExecutionStarted {
        node_id: NodeId,
        call: &'a FunctionCall,
    },

    /// Tool call requested (interceptable - can block/modify)
    ToolCallRequested { node_id: NodeId, call: FunctionCall },

    /// Tool execution completed (interceptable - can modify result)
    ToolCallCompleted {
        node_id: NodeId,
        call: FunctionCall,
        result: FunctionResult,
    },

    /// Tool execution failed (observable)
    ToolCallFailed {
        node_id: NodeId,
        call: &'a FunctionCall,
        error: String,
    },

    /// All tool results collected (observable)
    ToolBatchCompleted {
        node_id: NodeId,
        results: &'a Vec<FunctionResult>,
        duration: Duration,
    },

    /// Function call results (legacy, kept for compatibility)
    FunctionCallsResults {
        node_id: NodeId,
        calls: &'a Vec<FunctionCall>,
        results: &'a Vec<FunctionResult>,
    },

    /// Tool results being synthesized back to LLM (observable)
    ToolResultsSynthesisStarted {
        node_id: NodeId,
        result_count: usize,
    },

    // ============ COMPLETION EVENTS (Observable & Interceptable) ============
    /// About to return final result (interceptable)
    AboutToFinish { inference: &'a Inference },

    /// Agent execution finished (observable)
    Finished { inference: &'a Inference },

    /// Agent execution faulted (observable)
    Faulted {
        reason: String,
        iteration: usize,
        node_id: Option<NodeId>,
    },

    // ============ ERROR EVENTS (Observable) ============
    /// Generic error occurred (observable)
    Error { error: String },

    /// Recoverable error (execution continues)
    RecoverableError {
        error: String,
        recovery_action: String,
    },
}

/// Types of node updates
#[derive(Debug, Clone)]
pub enum NodeUpdateType {
    StatusChanged,
    DataModified,
    MetadataUpdated,
    ChildAdded { child_id: NodeId },
    ParentChanged { new_parent: Option<NodeId> },
}

/// Command from an interceptor listener
#[derive(Debug, Clone)]
pub enum InterceptionCommand {
    // Tool-related interceptions
    ToolCall(InterceptionToolCallCommand),

    // Inference-related interceptions
    Inference(InterceptionInferenceCommand),

    // History/Context-related interceptions
    History(InterceptionHistoryCommand),

    // Context management strategy interceptions
    ContextManagement(InterceptionContextManagementCommand),

    // LLM request interceptions
    LLMRequest(InterceptionLLMRequestCommand),

    // General flow control
    FlowControl(InterceptionFlowControlCommand),
}

#[derive(Debug, Clone)]
pub enum InterceptionToolCallCommand {
    /// Continue with original tool call
    Continue,

    /// Modify the tool call before execution
    Modify { call: FunctionCall },

    /// Block this tool call with a reason
    Block { reason: String },

    /// Replace the result without executing the tool
    ReplaceResult { result: FunctionResult },

    /// Execute the tool but transform its result
    TransformResult {
        transformer: String, // Name/ID of transformer to apply
    },
}

#[derive(Debug, Clone)]
pub enum InterceptionInferenceCommand {
    /// Continue with original inference
    Continue,

    /// Modify the inference before processing
    Modify { inference: Inference },

    /// Skip this inference entirely
    Skip,

    /// Replace the inference result without LLM call
    ReplaceResult { result: Inference },

    /// Add additional context to the inference
    EnrichContext { additional_context: String },
}

#[derive(Debug, Clone)]
pub enum InterceptionHistoryCommand {
    /// Continue with original history
    Continue,

    /// Replace entire history
    Replace { history: Vec<Inference> },

    /// Modify history (add/remove/edit messages)
    Modify { history: Vec<Inference> },

    /// Replace specific messages at given indices
    Inject {
        messages: Vec<(usize, Inference)>, // (index, message)
    },
}

#[derive(Debug, Clone)]
pub enum InterceptionContextManagementCommand {
    /// Continue with original history
    Continue,

    /// Skip context optimization for this iteration
    SkipOptimization,

    /// Apply different strategies than configured
    UseStrategies {
        strategies: Vec<ContextManagementStrategy>,
    },
}

#[derive(Debug, Clone)]
pub enum InterceptionLLMRequestCommand {
    /// Continue with original request
    Continue,

    /// Modify generation config
    ModifyConfig { config: GenerationConfig },

    /// Replace the entire request
    ReplaceRequest {
        inference: Inference,
        history: Vec<Inference>,
        config: GenerationConfig,
    },

    /// Skip LLM call and provide cached/mock Command
    UseCached { cached: Inference },
}

#[derive(Debug, Clone)]
pub enum InterceptionFlowControlCommand {
    /// Continue normal execution
    Continue,

    /// Pause execution (for debugging/inspection)
    Pause,

    /// Stop execution gracefully
    Stop { reason: String },

    /// Force retry current step
    Retry { max_attempts: usize },

    /// Jump to different execution path
    Redirect { target_node: NodeId },
}

/// Helper to categorize events
impl<'a> Event<'a> {
    /// Returns true if this event can be intercepted
    pub fn is_interceptable(&self) -> bool {
        matches!(
            self,
            Event::InferenceAboutToStart { .. }
                | Event::HistoryBuilt { .. }
                | Event::ContextOptimizationStarted { .. }
                | Event::LLMRequestAboutToSend { .. }
                | Event::ToolCallRequested { .. }
                | Event::ToolCallCompleted { .. }
                | Event::AboutToFinish { .. }
        )
    }

    /// Returns true if this is a lifecycle event
    pub fn is_lifecycle_event(&self) -> bool {
        matches!(
            self,
            Event::AgentInitialized { .. }
                | Event::AgentStatusChanged { .. }
                | Event::AgentReset { .. }
                | Event::TurnStarted { .. }
                | Event::TurnCompleted { .. }
        )
    }

    /// Returns true if this is a context-related event
    pub fn is_context_event(&self) -> bool {
        matches!(
            self,
            Event::HistoryBuilt { .. }
                | Event::ContextOptimizationStarted { .. }
                | Event::ContextStrategyApplied { .. }
                | Event::ContextOptimized { .. }
        )
    }

    /// Returns true if this is a tool-related event
    pub fn is_tool_event(&self) -> bool {
        matches!(
            self,
            Event::ToolBatchStarted { .. }
                | Event::ToolExecutionStarted { .. }
                | Event::ToolCallRequested { .. }
                | Event::ToolCallCompleted { .. }
                | Event::ToolCallFailed { .. }
                | Event::ToolBatchCompleted { .. }
                | Event::ToolResultsSynthesisStarted { .. }
        )
    }

    /// Returns true if this is an error event
    pub fn is_error_event(&self) -> bool {
        matches!(
            self,
            Event::Error { .. } | Event::RecoverableError { .. } | Event::Faulted { .. }
        )
    }

    /// Get event name for logging/debugging
    pub fn name(&self) -> &'static str {
        match self {
            Event::AgentInitialized { .. } => "AgentInitialized",
            Event::AgentStatusChanged { .. } => "AgentStatusChanged",
            Event::AgentReset { .. } => "AgentReset",
            Event::MaxIterationsWarning { .. } => "MaxIterationsWarning",
            Event::TurnStarted { .. } => "TurnStarted",
            Event::TurnCompleted { .. } => "TurnCompleted",
            Event::IterationStarted { .. } => "IterationStarted",
            Event::IterationCompleted { .. } => "IterationCompleted",
            Event::StepCompleted { .. } => "StepCompleted",
            Event::NodeCreated { .. } => "NodeCreated",
            Event::NodeStatusChanged { .. } => "NodeStatusChanged",
            Event::NodeUpdated { .. } => "NodeUpdated",
            Event::CurrentNodeChanged { .. } => "CurrentNodeChanged",
            Event::ConversationRootUpdated { .. } => "ConversationRootUpdated",
            Event::InputInference { .. } => "InputInference",
            Event::InferenceAboutToStart { .. } => "InferenceAboutToStart",
            Event::InferenceStarted { .. } => "InferenceStarted",
            Event::InferenceResultReceived { .. } => "InferenceResultReceived",
            Event::InferenceCompleted { .. } => "InferenceCompleted",
            Event::HistoryBuilt { .. } => "HistoryBuilt",
            Event::ContextOptimizationStarted { .. } => "ContextOptimizationStarted",
            Event::ContextStrategyApplied { .. } => "ContextStrategyApplied",
            Event::ContextOptimized { .. } => "ContextOptimized",
            Event::LLMRequestAboutToSend { .. } => "LLMRequestAboutToSend",
            Event::LLMRequestSent { .. } => "LLMRequestSent",
            Event::LLMRequestCompleted { .. } => "LLMRequestCompleted",
            Event::InferenceUsageMetadata { .. } => "InferenceUsageMetadata",
            Event::ToolBatchStarted { .. } => "ToolBatchStarted",
            Event::ToolExecutionStarted { .. } => "ToolExecutionStarted",
            Event::ToolCallRequested { .. } => "ToolCallRequested",
            Event::ToolCallCompleted { .. } => "ToolCallCompleted",
            Event::ToolCallFailed { .. } => "ToolCallFailed",
            Event::ToolBatchCompleted { .. } => "ToolBatchCompleted",
            Event::FunctionCallsResults { .. } => "FunctionCallsResults",
            Event::ToolResultsSynthesisStarted { .. } => "ToolResultsSynthesisStarted",
            Event::AboutToFinish { .. } => "AboutToFinish",
            Event::Finished { .. } => "Finished",
            Event::Faulted { .. } => "Faulted",
            Event::Error { .. } => "Error",
            Event::RecoverableError { .. } => "RecoverableError",
        }
    }
}
