use crate::Inference;
use crate::context::strategy::ContextManagementStrategy;

/// Context manager that handles conversation history optimization.
///
/// Supports two modes:
/// - Immutable: `apply()` returns a new history
/// - Mutable: `apply_mut()` modifies history in place
///
/// Strategies can be chained hierarchically by adding multiple strategies.
#[derive(Clone, Debug)]
pub struct ContextManager {
    strategies: Vec<ContextManagementStrategy>,
}

impl ContextManager {
    /// Create a new context manager with no strategies.
    pub fn new() -> Self {
        Self {
            strategies: Vec::new(),
        }
    }

    /// Create a context manager with a single strategy.
    pub fn with_strategy(strategy: ContextManagementStrategy) -> Self {
        Self {
            strategies: vec![strategy],
        }
    }

    /// Create a context manager with multiple strategies.
    pub fn with_strategies(strategies: Vec<ContextManagementStrategy>) -> Self {
        Self { strategies }
    }

    /// Add a strategy to the chain.
    pub fn add_strategy(&mut self, strategy: ContextManagementStrategy) {
        self.strategies.push(strategy);
    }

    /// Set all strategies at once, replacing existing ones.
    pub fn set_strategies(&mut self, strategies: Vec<ContextManagementStrategy>) {
        self.strategies = strategies;
    }

    /// Clear all strategies.
    pub fn clear_strategies(&mut self) {
        self.strategies.clear();
    }

    /// Get current strategies.
    pub fn strategies(&self) -> &[ContextManagementStrategy] {
        &self.strategies
    }

    /// Apply all strategies and return a new history (immutable).
    ///
    /// This creates a copy of the history and applies all strategies in sequence,
    /// returning the optimized version without modifying the original.
    pub async fn apply(&self, history: &[Inference]) -> crate::Result<Vec<Inference>> {
        let mut new_history = history.to_vec();
        self.apply_mut(&mut new_history).await?;
        Ok(new_history)
    }

    /// Apply all strategies to the history in place (mutable).
    ///
    /// This modifies the provided history directly, applying all strategies
    /// in the order they were added.
    pub async fn apply_mut(&self, history: &mut Vec<Inference>) -> crate::Result<()> {
        for strategy in &self.strategies {
            self.execute_strategy(strategy, history).await?;
        }
        Ok(())
    }

    async fn execute_strategy(
        &self,
        strategy: &ContextManagementStrategy,
        history: &mut Vec<Inference>,
    ) -> crate::Result<()> {
        match strategy {
            ContextManagementStrategy::TruncateByTurns { max_turns } => {
                self.truncate_by_turns(history, *max_turns)
            }

            ContextManagementStrategy::TruncateByChars { max_chars } => {
                self.truncate_by_chars(history, *max_chars)
            }

            ContextManagementStrategy::TruncateByTokens { max_tokens } => {
                self.truncate_by_tokens(history, *max_tokens)
            }

            ContextManagementStrategy::TruncateToolResults { max_length } => {
                self.truncate_tool_results(history, *max_length)
            }

            ContextManagementStrategy::TruncateToolResultsByDistance {
                truncation_factor,
                min_length,
            } => self.truncate_tool_results_by_distance(history, *truncation_factor, *min_length),

            ContextManagementStrategy::TruncateEverythingByDistance {
                truncation_factor,
                min_length,
            } => self.truncate_everything_by_distance(history, *truncation_factor, *min_length),

            ContextManagementStrategy::SlidingWindowWithPinnedMessages {
                window_size,
                pinned_indices,
            } => self.sliding_window_with_pins(history, *window_size, pinned_indices),

            ContextManagementStrategy::HierarchicalWindowing {
                immediate_window,
                recent_window,
                historical_samples,
            } => {
                self.hierarchical_windowing(
                    history,
                    *immediate_window,
                    *recent_window,
                    *historical_samples,
                )
                .await
            }

            ContextManagementStrategy::Summary { llm } => self.summary(history, llm.clone()).await,

            ContextManagementStrategy::SelectiveSummary {
                llm,
                summarize_after_turns,
                keep_recent,
            } => {
                self.selective_summary(history, llm.clone(), *summarize_after_turns, *keep_recent)
                    .await
            }

            ContextManagementStrategy::ProgressiveSummary {
                llm,
                first_layer_threshold,
                next_layer_threshold,
            } => {
                self.progressive_summary(
                    history,
                    llm.clone(),
                    *first_layer_threshold,
                    *next_layer_threshold,
                )
                .await
            }

            ContextManagementStrategy::SemanticClustering {
                llm,
                similarity_threshold,
                target_clusters,
                embedding_dim,
            } => {
                self.semantic_clustering(
                    history,
                    llm.clone(),
                    *similarity_threshold,
                    *target_clusters,
                    *embedding_dim,
                )
                .await
            }

            ContextManagementStrategy::ImportanceScoring {
                keep_top_n,
                weights,
            } => self.importance_scoring(history, *keep_top_n, weights),

            ContextManagementStrategy::SemanticRelevance {
                llm,
                top_k,
                keep_recent,
                embedding_dim,
            } => {
                self.semantic_relevance(history, llm.clone(), *top_k, *keep_recent, *embedding_dim)
                    .await
            }
        }
    }

    pub fn is_tool_result(&self, inf: &Inference) -> bool {
        inf.content
            .text
            .as_ref()
            .map(|t| t.contains("tool_result") || t.contains("function_result"))
            .unwrap_or(false)
    }

    pub fn has_tool_calls(&self, inf: &Inference) -> bool {
        inf.content
            .text
            .as_ref()
            .map(|t| t.contains("tool_call") || t.contains("function_call"))
            .unwrap_or(false)
    }

    pub fn cosine_similarity(&self, a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }

        dot_product / (norm_a * norm_b)
    }
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::new()
    }
}
