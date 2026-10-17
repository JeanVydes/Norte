//! The `models.dev` catalogue: where it lives, how big it is, and the compact
//! views the analyst models are given.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value, json};

/// The catalogue the example fetches out of the box.
pub(crate) const DEFAULT_URL: &str = "https://models.dev/catalog.json";

/// Keeps the fetched catalogue host-side between the tool call and the fan-out.
///
/// The document is several megabytes, so it is never handed to a model whole:
/// the agent sees a summary and the analysts see a bounded projection.
#[derive(Default)]
pub(crate) struct Cache {
    inner: Mutex<Option<Value>>,
}

impl Cache {
    /// Creates a shared, empty cache.
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replaces the cached catalogue.
    pub(crate) fn store(&self, catalog: Value) {
        *self.lock() = Some(catalog);
    }

    /// Returns a copy of the cached catalogue, if one was fetched.
    pub(crate) fn get(&self) -> Option<Value> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Value>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The number of providers and models in `catalog`.
pub(crate) fn counts(catalog: &Value) -> (usize, usize) {
    (
        section(catalog, "providers").map_or(0, Map::len),
        section(catalog, "models").map_or(0, Map::len),
    )
}

/// A bounded, analysis-ready view of every provider: who they are and how many
/// models they serve.
pub(crate) fn providers_view(catalog: &Value) -> Value {
    let providers = entries(catalog, "providers")
        .map(|(id, provider)| {
            json!({
                "id": id,
                "name": text(provider, "name"),
                "models": section(provider, "models").map_or(0, Map::len),
                "credentials": provider
                    .get("env")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            })
        })
        .collect::<Vec<_>>();
    Value::Array(providers)
}

/// A bounded, analysis-ready view of every model: the fields that describe a
/// model's capability envelope, without the prose or benchmarks.
pub(crate) fn models_view(catalog: &Value) -> Value {
    let models = entries(catalog, "models")
        .map(|(id, model)| {
            json!({
                "id": id,
                "name": text(model, "name"),
                "family": text(model, "family"),
                "reasoning": flag(model, "reasoning"),
                "tool_call": flag(model, "tool_call"),
                "context": model.pointer("/limit/context").cloned().unwrap_or(Value::Null),
                "input": model.pointer("/modalities/input").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    Value::Array(models)
}

fn section<'a>(value: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    value.get(key).and_then(Value::as_object)
}

fn entries<'a>(catalog: &'a Value, key: &str) -> impl Iterator<Item = (&'a str, &'a Value)> {
    section(catalog, key)
        .into_iter()
        .flatten()
        .map(|(id, value)| (id.as_str(), value))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or_default()
}
