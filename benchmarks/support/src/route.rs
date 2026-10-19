//! The route a live benchmark talks to: model, endpoint and provider pin.

use std::error::Error;
use std::time::Duration;

use norte_core::{Capabilities, ModelId, ModelRef, ProviderId, TurnOptions, WireFamily};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use serde::Serialize;

/// The default pause before each model request: long enough for a provider's
/// asynchronous prefix cache to publish the request before it.
pub const DEFAULT_PAUSE_MS: u64 = 2_000;

/// An `OpenAI`-compatible route, read from the environment.
#[derive(Debug, Clone, Serialize)]
pub struct Route {
    /// The model id.
    pub model: String,
    /// The base `URL`.
    pub endpoint: String,
    /// The upstream to pin through the router, when the run names one.
    pub provider: Option<String>,
    /// The environment variable that holds the key.
    pub key_env: String,
    /// Pause before each model request, in milliseconds. Zero disables it.
    pub pause_ms: u64,
}

impl Route {
    /// Reads the route from `BENCH_MODEL`, `BENCH_ENDPOINT`, the optional
    /// `BENCH_PROVIDER` and the key variable named by `BENCH_KEY_ENV`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first variable that is missing.
    pub fn from_env() -> Result<Self, Box<dyn Error>> {
        let pause_ms = std::env::var("BENCH_PAUSE_MS")
            .ok()
            .map(|raw| {
                raw.parse::<u64>().map_err(|_| {
                    format!("BENCH_PAUSE_MS must be a whole number of milliseconds, got `{raw}`")
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_PAUSE_MS);
        Ok(Self {
            model: required("BENCH_MODEL", "deepseek/deepseek-v4-flash-0731")?,
            endpoint: required("BENCH_ENDPOINT", "https://openrouter.ai/api/v1")?,
            provider: std::env::var("BENCH_PROVIDER").ok(),
            key_env: std::env::var("BENCH_KEY_ENV")
                .unwrap_or_else(|_| "OPENROUTER_API_KEY".to_owned()),
            pause_ms,
        })
    }

    /// The pause to take before each model request.
    #[must_use]
    pub fn pause(&self) -> Duration {
        Duration::from_millis(self.pause_ms)
    }

    /// Builds the `OpenAI`-chat adapter for this route.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is missing, the endpoint is not a valid
    /// `URL`, or the model id is empty.
    pub fn openai(&self, capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
        let provider = ProviderId::new("openrouter")?;
        let key = std::env::var(&self.key_env)
            .map_err(|_| format!("set {}, the key for {}", self.key_env, self.endpoint))?;
        let profile = VendorProfile::new(
            provider.clone(),
            WireFamily::new(WIRE_CHAT)?,
            &self.endpoint,
        )?
        .with_auth(AuthScheme::bearer(key));
        Ok(OpenAiChat::new(
            ModelRef::new(provider, ModelId::new(&self.model)?),
            profile,
            capabilities,
        )?)
    }

    /// The turn options a benchmark starts from: an output ceiling and an
    /// explicit route order when the run pins an upstream.
    ///
    /// Pinning is not about the price — the response carries that. It is what
    /// makes two runs comparable, because with a router free to choose, two
    /// turns of one conversation can land on different endpoints.
    #[must_use]
    pub fn turn_options(&self, max_output_tokens: u64) -> TurnOptions {
        let mut options = TurnOptions::default();
        options.params.max_output_tokens = Some(max_output_tokens);
        if let Some(provider) = &self.provider {
            options.extensions.insert(
                "openai.chat:provider".to_owned(),
                serde_json::json!({ "order": [provider], "allow_fallbacks": false }),
            );
        }
        options
    }

    /// A one-line description for a report.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} at {} ({}; pause {} ms)",
            self.model,
            self.endpoint,
            self.provider.as_deref().unwrap_or("the router decides"),
            self.pause_ms
        )
    }
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written down is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
///
/// # Errors
///
/// Returns a message naming the variable when it is not set.
pub fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}
