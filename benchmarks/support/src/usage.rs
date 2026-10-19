//! The provider's counters, summed the way a report needs them.

use std::collections::BTreeMap;

use norte_core::Usage;
use serde::Serialize;
use serde_json::Value;

use crate::time::percent;

/// Summed token counters and the provider's own bill over a set of responses.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Counters {
    /// Responses counted.
    pub requests: u64,
    /// Prompt tokens, cache reads included on `OpenAI`-compatible wires.
    pub input: u64,
    /// Tokens the model produced.
    pub output: u64,
    /// Reasoning tokens, where the provider separates them.
    pub reasoning: u64,
    /// Tokens the provider read from its cache.
    pub cache_read: u64,
    /// Tokens written into the provider's cache.
    pub cache_write: u64,
    /// Audio input tokens, where the provider counts them.
    pub audio_input: u64,
    /// Audio output tokens, where the provider counts them.
    pub audio_output: u64,
    /// Responses that carried no `cost` field.
    pub unpriced: u64,
    /// The sum of the `cost` fields the provider reported.
    pub cost: f64,
    /// A counter with no neutral field, summed by its vendor name.
    pub extension: BTreeMap<String, u64>,
    /// Whatever the vendor reported and this vocabulary does not model.
    pub vendor: BTreeMap<String, Value>,
}

impl Counters {
    /// Folds one response's usage in.
    pub fn add(&mut self, usage: &Usage) {
        self.requests += 1;
        self.input += usage.input_tokens.unwrap_or(0);
        self.output += usage.output_tokens.unwrap_or(0);
        self.reasoning += usage.reasoning_tokens.unwrap_or(0);
        self.cache_read += usage.cache_read_tokens.unwrap_or(0);
        self.cache_write += usage.cache_write_tokens.unwrap_or(0);
        self.audio_input += usage.audio_input_tokens.unwrap_or(0);
        self.audio_output += usage.audio_output_tokens.unwrap_or(0);
        match cost_of(usage) {
            Some(cost) => self.cost += cost,
            None => self.unpriced += 1,
        }
        for (key, value) in &usage.extension {
            *self.extension.entry(key.clone()).or_default() += value;
        }
        for (key, value) in &usage.vendor {
            self.vendor.insert(key.clone(), value.clone());
        }
    }

    /// Folds another tally in.
    pub fn merge(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.audio_input += other.audio_input;
        self.audio_output += other.audio_output;
        self.unpriced += other.unpriced;
        self.cost += other.cost;
        for (key, value) in &other.extension {
            *self.extension.entry(key.clone()).or_default() += value;
        }
        for (key, value) in &other.vendor {
            self.vendor.insert(key.clone(), value.clone());
        }
    }

    /// Input plus output, as far as the provider reported them.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output
    }

    /// The cache hit as a share of the input.
    #[must_use]
    pub fn hit_rate(&self) -> String {
        percent(self.cache_read.min(self.input), self.input)
    }

    /// The bill as a sentence, naming the requests that carried no cost.
    #[must_use]
    pub fn money(&self) -> String {
        if self.requests == 0 {
            return "no request was counted".to_owned();
        }
        if self.unpriced == 0 {
            format!("${:.6} over {} request(s)", self.cost, self.requests)
        } else {
            format!(
                "${:.6} over {} of {} request(s); {} reported no cost",
                self.cost,
                self.requests - self.unpriced,
                self.requests,
                self.unpriced
            )
        }
    }
}

/// The vendor's own `usage.cost`, when the payload carried one.
#[must_use]
pub fn cost_of(usage: &Usage) -> Option<f64> {
    usage.vendor.get("cost").and_then(Value::as_f64)
}
