use crate::Inference;
use crate::Role;
use crate::context::manager::ContextManager;
use crate::llm::definitions::LLMEmbedding;
use crate::prelude::InferenceContent;
use std::sync::Arc;

/// Context management strategies for handling conversation history with LLMs.
/// Organized from simple to complex approaches.
#[derive(Debug, Clone)]
pub enum ContextManagementStrategy {
    /// Keep only the most recent N turns in the conversation.
    TruncateByTurns { max_turns: usize },

    /// Truncate based on total character count.
    TruncateByChars { max_chars: usize },

    /// Truncate based on token count (more accurate for LLM context windows).
    TruncateByTokens { max_tokens: usize },

    /// Truncate only tool/function results to reduce verbosity.
    TruncateToolResults { max_length: usize },

    /// Progressively truncate tool results based on how far back they are.
    TruncateToolResultsByDistance {
        truncation_factor: f32,
        min_length: usize,
    },

    /// Truncate all content types based on their distance from the end.
    TruncateEverythingByDistance {
        truncation_factor: f32,
        min_length: usize,
    },

    /// Keep a sliding window with special handling for important messages.
    SlidingWindowWithPinnedMessages {
        window_size: usize,
        pinned_indices: Vec<usize>,
    },

    /// Maintain multiple temporal windows (immediate, recent, historical).
    HierarchicalWindowing {
        immediate_window: usize,
        recent_window: usize,
        historical_samples: usize,
    },

    /// Full conversation summarization using another LLM.
    Summary { llm: Arc<crate::LLM> },

    /// Selective summarization - only summarize portions based on age.
    SelectiveSummary {
        llm: Arc<crate::LLM>,
        summarize_after_turns: usize,
        keep_recent: usize,
    },

    /// Progressive summarization - create layered summaries.
    ProgressiveSummary {
        llm: Arc<crate::LLM>,
        first_layer_threshold: usize,
        next_layer_threshold: usize,
    },

    /// Group semantically similar messages and compress groups.
    SemanticClustering {
        llm: Arc<crate::LLM>,
        similarity_threshold: f32,
        target_clusters: usize,
        embedding_dim: i32,
    },

    /// Score messages by importance and keep the most important ones.
    ImportanceScoring {
        keep_top_n: usize,
        weights: ImportanceWeights,
    },

    /// Hybrid approach using embeddings to find relevant context.
    SemanticRelevance {
        llm: Arc<crate::LLM>,
        top_k: usize,
        keep_recent: usize,
        embedding_dim: i32,
    },
}

impl ContextManager {
    pub fn truncate_by_turns(
        &self,
        history: &mut Vec<Inference>,
        max_turns: usize,
    ) -> crate::Result<()> {
        if history.len() > max_turns {
            *history = history.split_off(history.len() - max_turns);
        }
        Ok(())
    }

    pub fn truncate_by_chars(
        &self,
        history: &mut Vec<Inference>,
        max_chars: usize,
    ) -> crate::Result<()> {
        let mut total_chars = 0;
        let mut keep_from = history.len();

        for (i, inf) in history.iter().enumerate().rev() {
            let msg_len = inf.content.text.as_ref().map(|t| t.len()).unwrap_or(0);
            total_chars += msg_len;

            if total_chars > max_chars {
                keep_from = i + 1;
                break;
            }
        }

        if keep_from > 0 {
            *history = history.split_off(keep_from);
        }

        Ok(())
    }

    pub fn truncate_by_tokens(
        &self,
        history: &mut Vec<Inference>,
        max_tokens: usize,
    ) -> crate::Result<()> {
        // Rough estimation: 1 token ≈ 4 characters in English
        let max_chars = max_tokens * 4;
        self.truncate_by_chars(history, max_chars)
    }

    pub fn truncate_tool_results(
        &self,
        history: &mut Vec<Inference>,
        max_length: usize,
    ) -> crate::Result<()> {
        for inf in history.iter_mut() {
            if self.is_tool_result(inf) {
                if let Some(text) = &mut inf.content.text {
                    if text.len() > max_length {
                        text.truncate(max_length);
                        text.push_str("... [truncated]");
                    }
                }
            }
        }
        Ok(())
    }

    pub fn truncate_tool_results_by_distance(
        &self,
        history: &mut Vec<Inference>,
        truncation_factor: f32,
        min_length: usize,
    ) -> crate::Result<()> {
        let total = history.len();

        for (i, inf) in history.iter_mut().enumerate() {
            if self.is_tool_result(inf) {
                let distance = (total - i - 1) as f32 / total.max(1) as f32;
                let truncation = 1.0 - (distance * truncation_factor);

                if let Some(text) = &mut inf.content.text {
                    let target_len =
                        (text.len() as f32 * truncation).max(min_length as f32) as usize;
                    if text.len() > target_len {
                        text.truncate(target_len);
                        text.push_str("... [truncated]");
                    }
                }
            }
        }
        Ok(())
    }

    pub fn truncate_everything_by_distance(
        &self,
        history: &mut Vec<Inference>,
        truncation_factor: f32,
        min_length: usize,
    ) -> crate::Result<()> {
        let total = history.len();

        for (i, inf) in history.iter_mut().enumerate() {
            let distance = (total - i - 1) as f32 / total.max(1) as f32;
            let truncation = 1.0 - (distance * truncation_factor);

            if let Some(text) = &mut inf.content.text {
                let target_len = (text.len() as f32 * truncation).max(min_length as f32) as usize;
                if text.len() > target_len {
                    text.truncate(target_len);
                    text.push_str("...");
                }
            }
        }
        Ok(())
    }

    pub fn sliding_window_with_pins(
        &self,
        history: &mut Vec<Inference>,
        window_size: usize,
        pinned_indices: &[usize],
    ) -> crate::Result<()> {
        if history.len() <= window_size {
            return Ok(());
        }

        let start_window = history.len() - window_size;
        let mut keep_indices: Vec<usize> = (start_window..history.len()).collect();

        for &idx in pinned_indices {
            if idx < start_window && !keep_indices.contains(&idx) {
                keep_indices.push(idx);
            }
        }

        keep_indices.sort_unstable();

        let new_history: Vec<Inference> = keep_indices
            .into_iter()
            .filter_map(|i| history.get(i).cloned())
            .collect();

        *history = new_history;
        Ok(())
    }

    pub async fn hierarchical_windowing(
        &self,
        history: &mut Vec<Inference>,
        immediate_window: usize,
        recent_window: usize,
        historical_samples: usize,
    ) -> crate::Result<()> {
        if history.len() <= immediate_window {
            return Ok(());
        }

        let mut result = Vec::new();
        let len = history.len();

        let historical_end = len.saturating_sub(immediate_window + recent_window);
        if historical_end > 0 {
            let sample_step =
                (historical_end as f32 / historical_samples.max(1) as f32).ceil() as usize;
            for i in (0..historical_end).step_by(sample_step.max(1)) {
                if let Some(msg) = history.get(i) {
                    result.push(msg.clone());
                }
            }
        }

        let recent_start = len.saturating_sub(immediate_window + recent_window);
        let recent_end = len.saturating_sub(immediate_window);
        for i in recent_start..recent_end {
            if let Some(msg) = history.get(i) {
                result.push(msg.clone());
            }
        }

        let immediate_start = len.saturating_sub(immediate_window);
        for i in immediate_start..len {
            if let Some(msg) = history.get(i) {
                result.push(msg.clone());
            }
        }

        *history = result;
        Ok(())
    }

    pub async fn summary(
        &self,
        history: &mut Vec<Inference>,
        llm: Arc<crate::LLM>,
    ) -> crate::Result<()> {
        if history.is_empty() {
            return Ok(());
        }

        let conversation_text = history
            .iter()
            .filter_map(|inf| {
                inf.content
                    .text
                    .as_ref()
                    .map(|text| format!("{:?}: {}", inf.content.role, text))
            })
            .collect::<Vec<String>>()
            .join("\n\n");

        let prompt = format!(
            "Summarize the following conversation concisely, preserving key information, \
            decisions made, and important context. Focus on facts and outcomes:\n\n{}",
            conversation_text
        );

        let summary = llm.inference(prompt.as_str()).generate().await?;

        *history = vec![Inference {
            content: InferenceContent {
                role: Role::System,
                text: Some(format!(
                    "[Summary of previous conversation]\n{}",
                    summary.content.text.unwrap_or_default()
                )),
                audio: None,
                images: None,
            },
            ..Default::default()
        }];

        Ok(())
    }

    pub async fn selective_summary(
        &self,
        history: &mut Vec<Inference>,
        llm: Arc<crate::LLM>,
        summarize_after_turns: usize,
        keep_recent: usize,
    ) -> crate::Result<()> {
        if history.len() <= keep_recent + summarize_after_turns {
            return Ok(());
        }

        let split_point = history.len() - keep_recent;
        let to_summarize = history.drain(..split_point).collect::<Vec<_>>();

        let conversation_text = to_summarize
            .iter()
            .filter_map(|inf| {
                inf.content
                    .text
                    .as_ref()
                    .map(|text| format!("{:?}: {}", inf.content.role, text))
            })
            .collect::<Vec<String>>()
            .join("\n\n");

        let prompt = format!(
            "Create a concise summary of this conversation segment, \
            keeping essential context and decisions:\n\n{}",
            conversation_text
        );

        let summary = llm.inference(prompt.as_str()).generate().await?;

        history.insert(
            0,
            Inference {
                content: InferenceContent {
                    role: Role::System,
                    text: Some(format!(
                        "[Summary]\n{}",
                        summary.content.text.unwrap_or_default()
                    )),
                    audio: None,
                    images: None,
                },
                ..Default::default()
            },
        );

        Ok(())
    }

    pub async fn progressive_summary(
        &self,
        history: &mut Vec<Inference>,
        llm: Arc<crate::LLM>,
        first_layer_threshold: usize,
        next_layer_threshold: usize,
    ) -> crate::Result<()> {
        let summary_count = history
            .iter()
            .filter(|inf| {
                inf.content
                    .text
                    .as_ref()
                    .map(|t| t.starts_with("[Summary"))
                    .unwrap_or(false)
            })
            .count();

        let non_summary_count = history.len() - summary_count;

        if summary_count == 0 && non_summary_count >= first_layer_threshold {
            self.selective_summary(
                history,
                llm,
                first_layer_threshold,
                first_layer_threshold / 2,
            )
            .await?;
        } else if summary_count > 0 && non_summary_count >= next_layer_threshold {
            let summaries: Vec<String> = history
                .iter()
                .filter_map(|inf| {
                    inf.content.text.as_ref().and_then(|t| {
                        if t.starts_with("[Summary") {
                            Some(t.clone())
                        } else {
                            None
                        }
                    })
                })
                .collect();

            if !summaries.is_empty() {
                let prompt = format!(
                    "Consolidate these summaries into a single, more concise summary:\n\n{}",
                    summaries.join("\n\n")
                );

                let meta_summary = llm.inference(prompt.as_str()).generate().await?;

                history.retain(|inf| {
                    !inf.content
                        .text
                        .as_ref()
                        .map(|t| t.starts_with("[Summary"))
                        .unwrap_or(false)
                });

                history.insert(
                    0,
                    Inference {
                        content: InferenceContent {
                            role: Role::System,
                            text: Some(format!(
                                "[Summary - Layer {}]\n{}",
                                summary_count + 1,
                                meta_summary.content.text.unwrap_or_default()
                            )),
                            audio: None,
                            images: None,
                        },
                        ..Default::default()
                    },
                );
            }
        }

        Ok(())
    }

    pub async fn semantic_clustering(
        &self,
        history: &mut Vec<Inference>,
        llm: Arc<crate::LLM>,
        similarity_threshold: f32,
        target_clusters: usize,
        embedding_dim: i32,
    ) -> crate::Result<()> {
        if history.len() <= target_clusters {
            return Ok(());
        }

        let mut embeddings: Vec<Vec<f32>> = Vec::new();
        let mut texts = Vec::new();

        for inf in history.iter() {
            if let Some(text) = &inf.content.text {
                if !text.is_empty() {
                    let embedding = llm.embedding(text, embedding_dim).await?;
                    embeddings.push(embedding);
                    texts.push(text.clone());
                }
            }
        }

        if embeddings.is_empty() {
            return Ok(());
        }

        let mut clusters: Vec<Vec<usize>> = Vec::new();
        let mut assigned = vec![false; embeddings.len()];

        for i in 0..embeddings.len() {
            if assigned[i] {
                continue;
            }

            let mut cluster = vec![i];
            assigned[i] = true;

            for j in (i + 1)..embeddings.len() {
                if !assigned[j] {
                    let similarity = self.cosine_similarity(&embeddings[i], &embeddings[j]);
                    if similarity >= similarity_threshold {
                        cluster.push(j);
                        assigned[j] = true;
                    }
                }
            }

            clusters.push(cluster);
        }

        clusters.sort_by_key(|c| std::cmp::Reverse(c.len()));
        clusters.truncate(target_clusters);

        let mut new_history = Vec::new();

        for cluster in clusters {
            if cluster.len() == 1 {
                if let Some(idx) = cluster.first() {
                    if let Some(inf) = history.get(*idx) {
                        new_history.push(inf.clone());
                    }
                }
            } else {
                let cluster_texts: Vec<String> = cluster
                    .iter()
                    .filter_map(|&idx| texts.get(idx).cloned())
                    .collect();

                let prompt = format!(
                    "Summarize these related messages into a single coherent message:\n\n{}",
                    cluster_texts.join("\n---\n")
                );

                let summary = llm.inference(prompt.as_str()).generate().await?;

                new_history.push(Inference {
                    content: InferenceContent {
                        role: Role::System,
                        text: Some(format!(
                            "[Clustered Summary]\n{}",
                            summary.content.text.unwrap_or_default()
                        )),
                        audio: None,
                        images: None,
                    },
                    ..Default::default()
                });
            }
        }

        *history = new_history;
        Ok(())
    }

    pub fn importance_scoring(
        &self,
        history: &mut Vec<Inference>,
        keep_top_n: usize,
        weights: &ImportanceWeights,
    ) -> crate::Result<()> {
        if history.len() <= keep_top_n {
            return Ok(());
        }

        let total = history.len() as f32;
        let mut metadata: Vec<MessageMetadata> = Vec::new();

        for (i, inf) in history.iter().enumerate() {
            let length = inf.content.text.as_ref().map(|t| t.len()).unwrap_or(0);
            let has_tools = self.is_tool_result(inf) || self.has_tool_calls(inf);

            let recency_score = (i as f32 / total) * weights.recency;
            let length_score = (length as f32).ln().max(0.0) * weights.length;
            let role_score = match inf.content.role {
                Role::User => weights.role_user,
                Role::Assistant => weights.role_assistant,
                Role::System => weights.role_system,
                _ => 0.5,
            };
            let tool_score = if has_tools { weights.has_tools } else { 0.0 };

            let total_score = recency_score + length_score + role_score + tool_score;

            metadata.push(MessageMetadata {
                index: i,
                score: total_score,
                length,
                role: inf.content.role.clone(),
                has_tools,
            });
        }

        metadata.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        metadata.truncate(keep_top_n);
        metadata.sort_by_key(|m| m.index);

        let new_history: Vec<Inference> = metadata
            .into_iter()
            .filter_map(|m| history.get(m.index).cloned())
            .collect();

        *history = new_history;
        Ok(())
    }

    pub async fn semantic_relevance(
        &self,
        history: &mut Vec<Inference>,
        llm: Arc<crate::LLM>,
        top_k: usize,
        keep_recent: usize,
        embedding_dim: i32,
    ) -> crate::Result<()> {
        if history.len() <= keep_recent + top_k {
            return Ok(());
        }

        let split_point = history.len() - keep_recent;
        let recent: Vec<Inference> = history.drain(split_point..).collect();

        let query = recent
            .iter()
            .rev()
            .find(|inf| matches!(inf.content.role, Role::User))
            .and_then(|inf| inf.content.text.clone())
            .unwrap_or_default();

        if query.is_empty() || history.is_empty() {
            *history = recent;
            return Ok(());
        }

        let query_embedding = llm.embedding(&query, embedding_dim).await?;

        let mut scored: Vec<(usize, f32, Inference)> = Vec::new();

        for (i, inf) in history.iter().enumerate() {
            if let Some(text) = &inf.content.text {
                if !text.is_empty() {
                    let embedding = llm.embedding(text, embedding_dim).await?;
                    let similarity = self.cosine_similarity(&query_embedding, &embedding);
                    scored.push((i, similarity, inf.clone()));
                }
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored.sort_by_key(|s| s.0);

        let mut relevant: Vec<Inference> = scored.into_iter().map(|s| s.2).collect();
        relevant.extend(recent);
        *history = relevant;

        Ok(())
    }
}

/// Weights for importance scoring heuristics.
#[derive(Debug, Clone, Copy)]
pub struct ImportanceWeights {
    pub recency: f32,
    pub length: f32,
    pub role_user: f32,
    pub role_assistant: f32,
    pub role_system: f32,
    pub has_tools: f32,
}

impl Default for ImportanceWeights {
    fn default() -> Self {
        Self {
            recency: 1.0,
            length: 0.3,
            role_user: 1.2,
            role_assistant: 0.8,
            role_system: 1.5,
            has_tools: 0.9,
        }
    }
}

/// Metadata about a message for importance scoring.
#[derive(Debug, Clone)]
pub struct MessageMetadata {
    pub index: usize,
    pub score: f32,
    pub length: usize,
    pub role: Role,
    pub has_tools: bool,
}
