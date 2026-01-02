use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crate::{
    Error, FunctionCall, FunctionResult, Inference, LLM,
    agent::{config::AgentConfig, status::AgentStatus},
    context::manager::ContextManager,
    execution::{
        context::Context,
        environment::ExecutionEnvironment,
        events::{
            Event, InterceptionContextManagementCommand, InterceptionFlowControlCommand, InterceptionHistoryCommand, InterceptionInferenceCommand, InterceptionLLMRequestCommand, InterceptionCommand, InterceptionToolCallCommand
        },
        listeners::{EventListener, ListenerRegistry},
        node::{ExecutionNode, NodeId, NodeStatus, NodeType},
    },
};

pub struct Agent<S, EV, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static,
    T: Send + Sync + 'static,
{
    pub name: String,
    pub llm: Arc<LLM>,
    pub context: Context<S, EV, T>,
    listeners: Arc<ListenerRegistry<S, EV, T>>,
    config: AgentConfig,
    status: AgentStatus,
    iterations: usize,
    turn_number: usize,
    root: Option<NodeId>,
}

impl<S, EV, T> Agent<S, EV, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static + Clone,
    T: Send + Sync + 'static + Clone,
{
    pub fn new(
        name: impl Into<String>,
        execution_id: S,
        llm: Arc<LLM>,
        runtime: EV,
        state: Arc<Mutex<T>>,
    ) -> Self {
        Self {
            name: name.into(),
            llm,
            context: Context::new(execution_id, runtime, state),
            listeners: Arc::new(ListenerRegistry::new()),
            config: AgentConfig::default(),
            status: AgentStatus::Idle,
            iterations: 0,
            turn_number: 0,
            root: None,
        }
    }

    /// Register a listener for observing events (non-blocking)
    pub async fn on_event(&self, listener: Arc<dyn EventListener<S, EV, T>>) {
        self.listeners.add_observer(listener).await;
    }

    /// Register a listener for intercepting and controlling execution
    pub async fn intercept(&self, listener: Arc<dyn EventListener<S, EV, T>>, priority: i32) {
        self.listeners.add_interceptor(listener, priority).await;
    }

    /// Advances the agent to the next step
    pub async fn next_step(&mut self, input: Option<Inference>) -> crate::Result<AgentStatus> {
        match (&self.status, input) {
            (AgentStatus::Idle, Some(inf)) => self.start_turn(inf).await,
            (AgentStatus::Executing, None) => self.execute_step().await,
            (AgentStatus::Finished(_) | AgentStatus::Faulted(_), Some(inf)) => {
                self.soft_reset().await;
                self.start_turn(inf).await
            }
            _ => Err(Error::Internal("Invalid state transition".into())),
        }
    }

    async fn start_turn(&mut self, input: Inference) -> crate::Result<AgentStatus> {
        self.turn_number += 1;

        // Emit turn started event
        self.emit_observable(Event::TurnStarted {
            turn_number: self.turn_number,
            inference: &input,
        })
        .await;

        self.emit_observable(Event::InputInference { inference: &input })
            .await;

        let parent = self.get_last_node().await;
        let node_id = self.create_node(NodeType::Inference(input), parent).await?;
        self.set_current_node(node_id).await;

        self.transition_status(AgentStatus::Executing).await;

        self.execute_step().await
    }

    async fn execute_step(&mut self) -> crate::Result<AgentStatus> {
        // Check iteration limit
        if self.iterations >= self.config.max_iterations {
            let reason = format!("Max iterations ({}) reached", self.config.max_iterations);

            self.emit_observable(Event::Faulted {
                reason: reason.clone(),
                iteration: self.iterations,
                node_id: self.context.current_node.read().await.clone(),
            })
            .await;

            return Ok(self.transition_status(AgentStatus::Faulted(reason)).await);
        }

        // Emit warning if approaching limit
        let remaining = self.config.max_iterations - self.iterations;
        if remaining <= self.config.iteration_warning_threshold {
            self.emit_observable(Event::MaxIterationsWarning {
                current: self.iterations,
                max: self.config.max_iterations,
                remaining,
            })
            .await;
        }

        self.iterations += 1;

        let current_id = self.get_current_node_id().await?;

        // Emit iteration started
        self.emit_observable(Event::IterationStarted {
            iteration: self.iterations,
            current_node: current_id.clone(),
        })
        .await;

        let iteration_start = Instant::now();
        let node = self.get_node(&current_id).await?;

        let result = match node.node_type {
            NodeType::Inference(inf) => self.process_inference_node(&current_id, inf).await,
            NodeType::ToolResults(_) => self.synthesize_results(&current_id).await,
            NodeType::InferenceResult(ref res) if res.has_function_calls() => {
                self.execute_tool_batch(&current_id, res.function_calls.clone())
                    .await
            }
            _ => Ok(self.status.clone()),
        };

        // Emit iteration completed
        self.emit_observable(Event::IterationCompleted {
            iteration: self.iterations,
            node_id: current_id,
            duration: iteration_start.elapsed(),
        })
        .await;

        result
    }

    #[async_recursion::async_recursion]
    async fn process_inference_node(
        &mut self,
        node_id: &NodeId,
        inference: Inference,
    ) -> crate::Result<AgentStatus> {
        // Emit about to start (interceptable)
        if let Some(response) = self
            .emit_interceptable(Event::InferenceAboutToStart {
                node_id: node_id.clone(),
                inference: &inference,
            })
            .await
        {
            match response {
                InterceptionCommand::Inference(InterceptionInferenceCommand::Skip) => {
                    return Ok(self.status.clone());
                }
                InterceptionCommand::Inference(InterceptionInferenceCommand::Modify {
                    inference: modified,
                }) => {
                    return self.process_inference_node(node_id, modified).await;
                }
                InterceptionCommand::Inference(InterceptionInferenceCommand::ReplaceResult {
                    result,
                }) => {
                    let result_id = self
                        .create_node(
                            NodeType::InferenceResult(result.clone()),
                            Some(node_id.clone()),
                        )
                        .await?;

                    self.set_current_node(result_id.clone()).await;
                    self.update_node_status(&result_id, NodeStatus::Completed)
                        .await?;
                    self.update_node_status(node_id, NodeStatus::Completed)
                        .await?;

                    return Ok(self
                        .transition_status(AgentStatus::Finished(Box::new(result)))
                        .await);
                }
                InterceptionCommand::FlowControl(InterceptionFlowControlCommand::Stop {
                    reason,
                }) => {
                    return Ok(self.transition_status(AgentStatus::Faulted(reason)).await);
                }
                _ => {}
            }
        }

        self.emit_observable(Event::InferenceStarted {
            node_id: node_id.clone(),
            inference: &inference,
        })
        .await;

        let inference_start = Instant::now();
        self.update_node_status(node_id, NodeStatus::Running)
            .await?;
        let node = self.get_node(node_id).await?;

        // Build history
        let mut history = if let Some(parent_id) = &node.parent {
            self.context
                .runtime
                .read()
                .await
                .to_history(parent_id)
                .await?
        } else {
            Vec::new()
        };

        let original_length = history.len();
        let estimated_tokens = self.estimate_tokens(&history);

        // Emit history built (interceptable)
        if let Some(response) = self
            .emit_interceptable(Event::HistoryBuilt {
                node_id: node_id.clone(),
                history: &history,
                message_count: original_length,
                estimated_tokens,
            })
            .await
        {
            match response {
                InterceptionCommand::History(InterceptionHistoryCommand::Replace {
                    history: new_history,
                }) => {
                    history = new_history;
                }
                InterceptionCommand::History(InterceptionHistoryCommand::Modify {
                    history: modified,
                }) => {
                    history = modified;
                }
                InterceptionCommand::History(InterceptionHistoryCommand::Inject { messages }) => {
                    for (pos, msg) in messages.into_iter().rev() {
                        history.insert(pos, msg);
                    }
                }
                _ => {}
            }
        }

        // Apply context management if configured
        if let Some(manager) = &self.config.context_management {
            let optimization_start = Instant::now();
            let strategies = manager.strategies();

            // Emit optimization started (interceptable)
            let should_optimize = if let Some(response) = self
                .emit_interceptable(Event::ContextOptimizationStarted {
                    original_length,
                    strategies,
                })
                .await
            {
                match response {
                    InterceptionCommand::ContextManagement(
                        InterceptionContextManagementCommand::SkipOptimization,
                    ) => false,
                    InterceptionCommand::ContextManagement(InterceptionContextManagementCommand::UseStrategies {
                        strategies: custom,
                    }) => {
                        // Apply custom strategies instead
                        let custom_manager = ContextManager::with_strategies(custom.clone());
                        custom_manager.apply_mut(&mut history).await?;
                        false // Skip normal optimization
                    }
                    _ => true,
                }
            } else {
                true
            };

            if should_optimize {
                // Apply each strategy and emit events
                for (idx, strategy) in strategies.iter().enumerate() {
                    let before_len = history.len();
                    let strategy_start = Instant::now();

                    let single_manager = ContextManager::with_strategy(strategy.clone());
                    single_manager.apply_mut(&mut history).await?;

                    let after_len = history.len();

                    self.emit_observable(Event::ContextStrategyApplied {
                        strategy_index: idx,
                        strategy_name: format!("{:?}", strategy),
                        before_length: before_len,
                        after_length: after_len,
                        duration: strategy_start.elapsed(),
                    })
                    .await;
                }
            }

            let optimized_length = history.len();
            let total_duration = optimization_start.elapsed();

            self.emit_observable(Event::ContextOptimized {
                original_length,
                optimized_length,
                applied_strategies: strategies.to_vec(),
                total_duration,
            })
            .await;
        }

        // Prepare LLM request
        let config = self.config.generation_config.clone();

        // Emit LLM request about to send (interceptable)
        let (final_inference, final_history, final_config) = if let Some(response) = self
            .emit_interceptable(Event::LLMRequestAboutToSend {
                inference: &inference,
                history: &history,
                config: &config,
            })
            .await
        {
            match response {
                InterceptionCommand::LLMRequest(
                    InterceptionLLMRequestCommand::ModifyConfig { config: new_config },
                ) => (inference.clone(), history.clone(), new_config),
                InterceptionCommand::LLMRequest(
                    InterceptionLLMRequestCommand::ReplaceRequest {
                        inference: new_inf,
                        history: new_hist,
                        config: new_conf,
                    },
                ) => (new_inf, new_hist, new_conf),
                InterceptionCommand::LLMRequest(
                    InterceptionLLMRequestCommand::UseCached { cached },
                ) => {
                    // Skip LLM call entirely
                    let result_id = self
                        .create_node(
                            NodeType::InferenceResult(cached.clone()),
                            Some(node_id.clone()),
                        )
                        .await?;

                    self.set_current_node(result_id.clone()).await;
                    self.update_node_status(&result_id, NodeStatus::Completed)
                        .await?;
                    self.update_node_status(node_id, NodeStatus::Completed)
                        .await?;

                    if cached.has_function_calls() {
                        return self
                            .execute_tool_batch(&result_id, cached.function_calls)
                            .await;
                    } else {
                        return Ok(self
                            .transition_status(AgentStatus::Finished(Box::new(cached)))
                            .await);
                    }
                }
                _ => (inference.clone(), history.clone(), config),
            }
        } else {
            (inference, history, config)
        };

        // Emit LLM request sent
        self.emit_observable(Event::LLMRequestSent {
            message_count: final_history.len() + 1,
            estimated_tokens: self.estimate_tokens(&final_history),
            model: self.llm.model.clone(),
        })
        .await;

        let llm_start = Instant::now();

        // Make LLM call
        let result = match self
            .llm
            .inference(final_inference)
            .with_history(&final_history)
            .with_config(final_config)
            .generate()
            .await
        {
            Ok(res) => res,
            Err(e) => {
                self.transition_status(AgentStatus::Faulted(format!("LLM request failed: {}", e)))
                    .await;

                return Err(e);
            }
        };

        let llm_duration = llm_start.elapsed();

        // Emit LLM request completed
        self.emit_observable(Event::LLMRequestCompleted {
            result: &result,
            duration: llm_duration,
        })
        .await;

        self.emit_observable(Event::InferenceUsageMetadata(result.usage.clone()))
            .await;

        let result_id = self
            .create_node(
                NodeType::InferenceResult(result.clone()),
                Some(node_id.clone()),
            )
            .await?;

        self.emit_observable(Event::InferenceResultReceived {
            node_id: result_id.clone(),
            inference: &result,
        })
        .await;

        self.set_current_node(result_id.clone()).await;
        self.update_node_status(&result_id, NodeStatus::Completed)
            .await?;
        self.update_node_status(node_id, NodeStatus::Completed)
            .await?;

        let inference_duration = inference_start.elapsed();

        self.emit_observable(Event::InferenceCompleted {
            node_id: result_id.clone(),
            inference: &result,
            duration: inference_duration,
        })
        .await;

        self.emit_observable(Event::StepCompleted {
            step: self.iterations,
            node_id: result_id.clone(),
        })
        .await;

        if result.has_function_calls() {
            self.execute_tool_batch(&result_id, result.function_calls)
                .await
        } else {
            self.update_conversation_root().await;

            // Emit about to finish (interceptable)
            if let Some(response) = self
                .emit_interceptable(Event::AboutToFinish { inference: &result })
                .await
            {
                match response {
                    InterceptionCommand::FlowControl(InterceptionFlowControlCommand::Stop {
                        reason,
                    }) => {
                        return Ok(self.transition_status(AgentStatus::Faulted(reason)).await);
                    }
                    _ => {}
                }
            }

            self.emit_observable(Event::Finished { inference: &result })
                .await;

            // Emit turn completed
            self.emit_observable(Event::TurnCompleted {
                turn_number: self.turn_number,
                total_iterations: self.iterations,
                final_node: result_id,
            })
            .await;

            Ok(self
                .transition_status(AgentStatus::Finished(Box::new(result)))
                .await)
        }
    }

    #[async_recursion::async_recursion]
    async fn execute_tool_batch(
        &mut self,
        parent_id: &NodeId,
        calls: Vec<FunctionCall>,
    ) -> crate::Result<AgentStatus> {
        let tool_names: Vec<String> = calls.iter().map(|c| c.name.clone()).collect();

        // Emit tool batch started
        self.emit_observable(Event::ToolBatchStarted {
            node_id: parent_id.clone(),
            call_count: calls.len(),
            tool_names,
        })
        .await;

        let batch_start = Instant::now();
        let mut executable_calls = Vec::new();

        for call in calls {
            let event = Event::ToolCallRequested {
                node_id: parent_id.clone(),
                call: call.clone(),
            };

            match self.emit_interceptable(event).await {
                Some(InterceptionCommand::ToolCall(InterceptionToolCallCommand::Modify {
                    call: modified,
                })) => {
                    executable_calls.push(modified);
                }
                Some(InterceptionCommand::ToolCall(InterceptionToolCallCommand::Block {
                    reason,
                })) => {
                    log::debug!("Tool call blocked: {}", reason);
                }
                Some(InterceptionCommand::ToolCall(
                    InterceptionToolCallCommand::ReplaceResult { result: _ },
                )) => {
                    log::trace!("Replace not supported for ToolCallRequested");
                }
                None
                | Some(InterceptionCommand::ToolCall(InterceptionToolCallCommand::Continue)) => {
                    executable_calls.push(call);
                }
                _ => {
                    executable_calls.push(call);
                }
            }
        }

        if executable_calls.is_empty() {
            return self.synthesize_results(parent_id).await;
        }

        let results = self
            .execute_tools(executable_calls.clone(), parent_id)
            .await?;

        let batch_duration = batch_start.elapsed();

        // ensure ordering results in the same way as calls, some providers like mistral, require strictal ordering
        let results = executable_calls
            .iter()
            .filter_map(|call| {
                results
                    .iter()
                    .find(|res| res.name == call.name && res.context == call.context)
                    .cloned()
            })
            .collect::<Vec<FunctionResult>>();

        let mut result_node_id: NodeId = parent_id.clone();
        // we create a node per result, some providers like mistral, require individual message for each tool call
        for result in results.iter() {
            result_node_id = self
                .create_node(
                    NodeType::ToolResults(vec![result.clone()]),
                    // since we create multiple nodes, chain them as parent-child
                    Some(result_node_id.clone()),
                )
                .await?;
        }

        // Emit tool batch completed
        self.emit_observable(Event::ToolBatchCompleted {
            node_id: result_node_id.clone(),
            results: &results,
            duration: batch_duration,
        })
        .await;

        self.emit_observable(Event::FunctionCallsResults {
            node_id: result_node_id.clone(),
            calls: &executable_calls,
            results: &results,
        })
        .await;

        self.set_current_node(result_node_id.clone()).await;
        self.update_node_status(&result_node_id, NodeStatus::Completed)
            .await?;

        self.execute_step().await
    }

    async fn execute_tools(
        &self,
        calls: Vec<FunctionCall>,
        node_id: &NodeId,
    ) -> crate::Result<Vec<FunctionResult>> {
        let futures: Vec<_> = calls
            .into_iter()
            .map(|call| {
                let llm = self.llm.clone();
                let node_id = node_id.clone();
                let listeners = self.listeners.clone();
                let context = self.context.clone();

                async move {
                    // Emit tool execution started
                    listeners
                        .emit_observable(
                            &Event::ToolExecutionStarted {
                                node_id: node_id.clone(),
                                call: &call,
                            },
                            &context,
                        )
                        .await;

                    let res = if let Some(tool) = llm.get_tool(&call.name) {
                        match tool.execute(&call.arguments).await {
                            Ok(r) => r,
                            Err(e) => {
                                // Emit tool failed event
                                listeners
                                    .emit_observable(
                                        &Event::ToolCallFailed {
                                            node_id: node_id.clone(),
                                            call: &call,
                                            error: e.to_string(),
                                        },
                                        &context,
                                    )
                                    .await;

                                serde_json::json!({ "error": e.to_string() })
                            }
                        }
                    } else {
                        let error = "Tool not found";

                        listeners
                            .emit_observable(
                                &Event::ToolCallFailed {
                                    node_id: node_id.clone(),
                                    call: &call,
                                    error: error.to_string(),
                                },
                                &context,
                            )
                            .await;

                        serde_json::json!({ "error": error })
                    };

                    let result = FunctionResult {
                        name: call.name.clone(),
                        results: res,
                        context: call.context.clone(),
                    };

                    // Emit completed event (can be intercepted to modify result)
                    let event = Event::ToolCallCompleted {
                        node_id,
                        call,
                        result: result.clone(),
                    };

                    match listeners.emit_interceptable(&event, &context).await {
                        Some(InterceptionCommand::ToolCall(
                            InterceptionToolCallCommand::ReplaceResult { result: new_result },
                        )) => new_result,
                        _ => result,
                    }
                }
            })
            .collect();

        Ok(futures_util::future::join_all(futures).await)
    }

    async fn synthesize_results(&mut self, parent_id: &NodeId) -> crate::Result<AgentStatus> {
        let node = self.get_node(parent_id).await?;
        if let NodeType::ToolResults(results) = node.node_type {
            // Emit synthesis started
            self.emit_observable(Event::ToolResultsSynthesisStarted {
                node_id: parent_id.clone(),
                result_count: results.len(),
            })
            .await;

            let tool_inference = Inference::with_function_results(results);
            self.process_inference_node(parent_id, tool_inference).await
        } else {
            Err(Error::Internal("No tool results to synthesize".into()))
        }
    }

    /// Emit an observable event to all listeners
    async fn emit_observable<'a>(&self, event: Event<'a>) {
        self.listeners.emit_observable(&event, &self.context).await;
    }

    /// Emit an interceptable event and get response
    async fn emit_interceptable<'a>(&self, event: Event<'a>) -> Option<InterceptionCommand> {
        self.listeners
            .emit_interceptable(&event, &self.context)
            .await
    }

    async fn transition_status(&mut self, new_status: AgentStatus) -> AgentStatus {
        self.emit_observable(Event::AgentStatusChanged {
            from: self.status.clone(),
            to: new_status.clone(),
        })
        .await;

        self.status = new_status;
        self.status.clone()
    }

    async fn create_node(
        &self,
        node_type: NodeType,
        parent: Option<NodeId>,
    ) -> crate::Result<NodeId> {
        let node = ExecutionNode::new(node_type.clone(), parent.clone());
        let node_id = self.context.runtime.read().await.add_node(node).await?;

        // Emit node created event
        self.emit_observable(Event::NodeCreated {
            node_id: node_id.clone(),
            node_type,
            parent,
        })
        .await;

        Ok(node_id)
    }

    async fn get_node(&self, id: &NodeId) -> crate::Result<ExecutionNode> {
        let runtime = self.context.runtime.read().await;
        let nodes = runtime.get_nodes().await;
        nodes
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound("Node not found".into()))
    }

    async fn get_current_node_id(&self) -> crate::Result<NodeId> {
        self.context
            .current_node
            .read()
            .await
            .clone()
            .ok_or_else(|| Error::Internal("No current node".into()))
    }

    async fn set_current_node(&self, id: NodeId) {
        let old = self.context.current_node.read().await.clone();
        *self.context.current_node.write().await = Some(id.clone());

        // Emit current node changed
        self.emit_observable(Event::CurrentNodeChanged { from: old, to: id })
            .await;
    }

    async fn update_node_status(&self, id: &NodeId, status: NodeStatus) -> crate::Result<()> {
        let old_status = self.get_node(id).await?.status;

        self.context
            .update_node(id, |n| n.status = status.clone())
            .await?;

        if old_status != status {
            self.emit_observable(Event::NodeStatusChanged {
                node_id: id.clone(),
                from: old_status,
                to: status,
            })
            .await;
        }

        Ok(())
    }

    async fn get_last_node(&self) -> Option<NodeId> {
        if self.root.is_some() {
            self.context.current_node.read().await.clone()
        } else {
            None
        }
    }

    async fn update_conversation_root(&mut self) {
        if let Ok(id) = self.get_current_node_id().await {
            let previous = self.root.clone();
            self.root = Some(id.clone());

            self.emit_observable(Event::ConversationRootUpdated {
                previous,
                new_root: id,
            })
            .await;
        }
    }

    fn estimate_tokens(&self, history: &[Inference]) -> usize {
        // Rough estimation: 1 token ≈ 4 characters
        history
            .iter()
            .filter_map(|inf| inf.content.text.as_ref())
            .map(|text| text.len() / 4)
            .sum()
    }

    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn soft_reset(&mut self) {
        self.status = AgentStatus::Idle;
        self.iterations = 0;

        self.emit_observable(Event::AgentReset {
            hard: false,
            previous_root: self.root.clone(),
        })
        .await;
    }

    pub async fn hard_reset(&mut self) -> crate::Result<()> {
        let previous_root = self.root.clone();

        self.status = AgentStatus::Idle;
        self.iterations = 0;
        self.root = None;
        self.turn_number = 0;
        *self.context.current_node.write().await = None;
        self.context.runtime.read().await.clear().await?;

        self.emit_observable(Event::AgentReset {
            hard: true,
            previous_root,
        })
        .await;

        Ok(())
    }
}
