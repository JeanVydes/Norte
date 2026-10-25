//! A layer: wrapping one model call, with no engine and no session.
//!
//! A `Layer` sits between the caller and the `Model` port. It sees the request
//! about to go out and decides what to do with it: forward it, rewrite it, or
//! answer with a stream of its own and never reach the model at all.
//!
//! That last case is why a layer reports nothing. A stream that never reached
//! the model *is* the short circuit — there is no event to emit and no session
//! to emit it into. The layer decides; the caller sees the difference.
//!
//! ```sh
//! cargo run -p norte-cookbook --example layer
//! ```

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt;
use norte_core::{
    Continuation, Layer, LayerContext, LayerStack, Message, Model, ModelRequest, ModelStream,
    Result, StreamEvent, synthetic_stream,
};
use norte_testing::{ScriptedModel, model};

/// The terminal call: hands the request to the port, which is the model.
struct Adapter(Arc<dyn Model>);

#[async_trait::async_trait]
impl Continuation for Adapter {
    async fn call(&self, _context: &LayerContext, request: ModelRequest) -> Result<ModelStream> {
        self.0.stream(request).await
    }
}

/// Answers from a map when it has seen the same request before.
struct Cache {
    seen: Mutex<BTreeMap<String, Vec<StreamEvent>>>,
    hits: Mutex<usize>,
}

impl Cache {
    fn new() -> Self {
        Self {
            seen: Mutex::new(BTreeMap::new()),
            hits: Mutex::new(0),
        }
    }

    fn key(request: &ModelRequest) -> String {
        // A real cache would digest the wire body; the text of the conversation
        // is enough to make the point here.
        request
            .messages
            .iter()
            .map(Message::text)
            .collect::<Vec<_>>()
            .join("\u{1f}")
    }
}

#[async_trait::async_trait]
impl Layer for Cache {
    fn name(&self) -> &'static str {
        "cache"
    }

    async fn wrap(
        &self,
        context: &LayerContext,
        request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream> {
        let key = Self::key(&request);
        if let Some(events) = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            *self.hits.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            println!(
                "  [cache] hit at turn {} — the model is not called",
                context.turn
            );
            return Ok(synthetic_stream(events.clone()));
        }

        // Collecting to cache means draining the stream; a real cache would tee
        // it instead so the caller still sees it arrive live.
        let mut stream = next.call(context, request).await?;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event?);
        }
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, events.clone());
        Ok(synthetic_stream(events))
    }
}

/// Counts the requests that reached it, and rewrites one parameter.
struct Downstream {
    forwarded: Mutex<usize>,
}

#[async_trait::async_trait]
impl Layer for Downstream {
    fn name(&self) -> &'static str {
        "downstream"
    }

    async fn wrap(
        &self,
        context: &LayerContext,
        mut request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream> {
        *self
            .forwarded
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        // The layer changes the request before the model sees it. Nothing above
        // it learns that the temperature is not the one it asked for.
        request.params.temperature = Some(0.2);
        next.call(context, request).await
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let model: Arc<dyn Model> = Arc::new(
        ScriptedModel::new(model("cookbook/scripted"))
            .turn_text("A prompt cache keeps the shared prefix warm."),
    );

    let cache = Arc::new(Cache::new());
    let downstream = Arc::new(Downstream {
        forwarded: Mutex::new(0),
    });
    let layers = LayerStack::new(vec![
        Arc::clone(&cache) as Arc<dyn Layer>,
        Arc::clone(&downstream) as Arc<dyn Layer>,
    ]);
    let adapter = Adapter(Arc::clone(&model));

    let request = ModelRequest::new(
        model.reference().clone(),
        vec![Message::user("What is a prompt cache?")],
    );

    // The stack runs around the adapter directly: no `Harness`, no store, no
    // commit. A layer wraps a *model call*, and that is all this needs.
    for turn in 1..=2 {
        println!("turn {turn}");
        let context = LayerContext {
            session: norte_core::id::SessionId::new(),
            turn,
        };
        let mut stream = layers.run(&context, request.clone(), &adapter).await?;
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta { text, .. } = event? {
                print!("{text}");
            }
        }
        println!("\n");
    }

    println!(
        "forwarded to the model {} time(s), served from cache {} time(s)",
        downstream
            .forwarded
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        cache.hits.lock().unwrap_or_else(PoisonError::into_inner),
    );

    Ok(())
}
