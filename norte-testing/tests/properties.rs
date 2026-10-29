//! Property tests for the invariants the requirements ledger pins (FR-TEST-3): continuity merge,
//! graph commits, reconstruction determinism / prefix stability (FR-RECON-1,
//! FR-RECON-2, FR-RECON-11) and lossless, byte-stable conversation
//! export/import (FR-EXP-2).
//!
//! The generator is a small deterministic LCG so failures reproduce exactly and
//! the suite needs no extra dependency.

use std::sync::Arc;

use norte_anthropic::{AnthropicContinuity, WIRE_MESSAGES};
use norte_core::{
    ContinuityAdapter, ContinuityFragment, ContinuityScope, ContinuitySemantics, ContinuityToken,
    Digest, Message, Model, ModelRef, ModelRequest, WireFamily,
};
use norte_gemini::{GeminiContinuity, WIRE_GENERATE};
use norte_harness::Conversation;
use norte_harness::Store;
use norte_harness::{Harness, HarnessBuilder};
use norte_openai::{OpenAiContinuity, WIRE_RESPONSES};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model, model_conformance};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 16
    }

    fn bounded(&mut self, bound: u64) -> usize {
        usize::try_from(self.next() % bound.max(1)).expect("bounded fits usize")
    }

    fn text(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(b'a' + u8::try_from(self.next() % 26).expect("ascii")))
            .collect()
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len)
            .map(|_| u8::try_from(self.next() % 256).expect("byte"))
            .collect()
    }
}

fn fragment(family: &WireFamily, model: &ModelRef, bytes: Vec<u8>) -> ContinuityFragment {
    ContinuityFragment::new(
        family.clone(),
        ContinuityScope::Exact(model.clone()),
        1,
        ContinuitySemantics::Append,
        bytes,
    )
}

fn folded(
    adapter: &dyn ContinuityAdapter,
    family: &WireFamily,
    model: &ModelRef,
    chunks: &[Vec<u8>],
) -> ContinuityToken {
    let mut token = None;
    for chunk in chunks {
        token = Some(
            adapter
                .merge(token, fragment(family, model, chunk.clone()))
                .expect("merge"),
        );
    }
    token.expect("at least one chunk")
}

fn assert_merge_properties(
    adapter: &dyn ContinuityAdapter,
    family: &WireFamily,
    model: &ModelRef,
    chunks: &[Vec<u8>],
) {
    let whole = folded(adapter, family, model, chunks);
    assert_eq!(whole, folded(adapter, family, model, chunks));
    let expected: Vec<u8> = chunks.iter().flatten().copied().collect();
    assert_eq!(whole.payload(), expected.as_slice());

    let split = (chunks.len() / 2).max(1);
    let mut batched = folded(adapter, family, model, &chunks[..split]);
    for chunk in &chunks[split..] {
        batched = adapter
            .merge(Some(batched), fragment(family, model, chunk.clone()))
            .expect("merge");
    }
    assert_eq!(batched, whole);
}

fn continuity_adapters(model: &ModelRef) -> Vec<(WireFamily, Box<dyn ContinuityAdapter>)> {
    let openai = WireFamily::new(WIRE_RESPONSES).expect("family");
    let anthropic = WireFamily::new(WIRE_MESSAGES).expect("family");
    let gemini = WireFamily::new(WIRE_GENERATE).expect("family");
    vec![
        (
            openai.clone(),
            Box::new(OpenAiContinuity::new(openai, model.clone())),
        ),
        (
            anthropic.clone(),
            Box::new(AnthropicContinuity::new(anthropic, model.clone())),
        ),
        (
            gemini.clone(),
            Box::new(GeminiContinuity::new(gemini, model.clone())),
        ),
    ]
}

#[test]
fn every_adapter_merges_continuity_deterministically() {
    let reference = model("openai/gpt-5");
    let adapters = continuity_adapters(&reference);
    let mut rng = Rng::new(7);
    for _ in 0..32 {
        let chunks: Vec<Vec<u8>> = (0..=rng.bounded(4))
            .map(|_| {
                let len = 1 + rng.bounded(8);
                rng.bytes(len)
            })
            .collect();
        for (family, adapter) in &adapters {
            assert_merge_properties(adapter.as_ref(), family, &reference, &chunks);
        }
    }
}

#[test]
fn every_adapter_satisfies_the_model_conformance_suite() {
    let capabilities = || {
        norte_core::Capabilities::builder()
            .input(norte_core::Modality::Text)
            .output(norte_core::Modality::Text)
            .build()
    };

    let openai = norte_openai::presets::openai("gpt-5", "sk-test", capabilities()).expect("openai");
    model_conformance(&openai, openai.reference());

    let anthropic =
        norte_anthropic::presets::anthropic("claude-sonnet-4", "sk-test", capabilities())
            .expect("anthropic");
    model_conformance(&anthropic, anthropic.reference());

    let gemini =
        norte_gemini::presets::gemini("gemini-2.5-pro", "sk-test", capabilities()).expect("gemini");
    model_conformance(&gemini, gemini.reference());
}

#[test]
fn a_foreign_fragment_is_a_typed_error() {
    let reference = model("openai/gpt-5");
    let family = WireFamily::new(WIRE_RESPONSES).expect("family");
    let adapter = OpenAiContinuity::new(family, reference);
    let foreign = ContinuityFragment::new(
        WireFamily::new("other.wire").expect("family"),
        ContinuityScope::Any,
        1,
        ContinuitySemantics::Append,
        b"x".to_vec(),
    );
    assert!(matches!(
        adapter.merge(None, foreign),
        Err(norte_core::Error::Codec(_))
    ));
}

fn digest_of(request: &ModelRequest) -> Digest {
    Digest::of_json(&serde_json::to_value(request).expect("json"))
}

fn scripted(turns: usize, rng: &mut Rng) -> Arc<ScriptedModel> {
    let mut model = ScriptedModel::new(model("test/echo"));
    for _ in 0..turns {
        model = model.turn_text(rng.text(8));
    }
    Arc::new(model)
}

struct World {
    engine: Harness,
    store: Arc<InMemoryStore>,
    model: Arc<ScriptedModel>,
}

fn world(turns: usize, rng: &mut Rng) -> World {
    let store = Arc::new(InMemoryStore::new());
    let model = scripted(turns, rng);
    let engine = HarnessBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(model.clone() as Arc<dyn Model>)
        .build()
        .expect("engine");
    World {
        engine,
        store,
        model,
    }
}

async fn drain(world: &World, turns: usize, rng: &mut Rng) -> Vec<Message> {
    let session = world.engine.create().await.expect("create");
    let mut messages = Vec::new();
    for _ in 0..turns {
        let outcome = world
            .engine
            .send(session.id, Message::user(rng.text(6)))
            .finish()
            .await
            .expect("turn");
        messages.push(outcome.message.expect("assistant message"));
    }
    messages
}

#[tokio::test]
async fn request_reconstruction_is_deterministic_and_attributable() {
    let mut rng = Rng::new(11);
    let world = world(6, &mut rng);
    let messages = drain(&world, 6, &mut rng).await;
    let requests = world.model.requests();
    assert_eq!(requests.len(), 6);

    for (request, message) in requests.iter().zip(&messages) {
        let stored = message
            .origin
            .as_ref()
            .and_then(|origin| origin.request_digest)
            .expect("digest recorded");
        assert_eq!(stored, digest_of(request));
        assert_eq!(digest_of(request), digest_of(request));
    }
}

#[tokio::test]
async fn appending_a_turn_leaves_the_cached_prefix_stable() {
    let mut rng = Rng::new(23);
    let world = world(6, &mut rng);
    drain(&world, 6, &mut rng).await;
    let requests = world.model.requests();

    for window in requests.windows(2) {
        let previous = &window[0];
        let current = &window[1];

        let prefix = previous.messages.len();
        assert_eq!(current.messages[..prefix], previous.messages[..]);

        for index in 0..previous.cache_boundaries.len().saturating_sub(1) {
            assert_eq!(
                current.cache_boundaries[index], previous.cache_boundaries[index],
                "an interior cache boundary moved when a turn was appended"
            );
        }
        let end = u32::try_from(current.messages.len()).expect("length fits u32");
        assert_eq!(current.cache_boundaries.last().copied(), Some(end));
    }
}

#[tokio::test]
async fn graph_commits_are_linear_history_and_ancestors_agree() {
    let mut rng = Rng::new(41);
    let world = world(5, &mut rng);
    let session = world.engine.create().await.expect("create");
    let mut previous_version = session.version;
    let mut heads = Vec::new();
    for _ in 0..5 {
        let outcome = world
            .engine
            .send(session.id, Message::user(rng.text(6)))
            .finish()
            .await
            .expect("turn");
        assert!(outcome.session.version > previous_version);
        previous_version = outcome.session.version;
        heads.push(outcome.session.head.expect("head"));
    }

    for (index, head) in heads.iter().enumerate() {
        let history = world.store.history(Some(*head)).await.expect("history");
        assert_eq!(history.len(), 2 * (index + 1));
        let ancestors = world.store.ancestors(*head).await.expect("ancestors");
        assert_eq!(ancestors.len(), history.len());
        assert_eq!(ancestors.last().copied(), Some(*head));
        if index > 0 {
            let previous = world
                .store
                .history(Some(heads[index - 1]))
                .await
                .expect("history");
            assert_eq!(history[..previous.len()], previous[..]);
        }
    }
}

#[tokio::test]
async fn conversation_export_import_is_lossless_and_byte_stable() {
    let mut rng = Rng::new(53);
    let world = world(5, &mut rng);
    let session = world.engine.create().await.expect("create");
    for _ in 0..5 {
        world
            .engine
            .send(session.id, Message::user(rng.text(6)))
            .finish()
            .await
            .expect("turn");
    }

    let conversation = world.engine.export(session.id).await.expect("export");
    let json = conversation.to_json().expect("to_json");
    let decoded = Conversation::from_json(&json).expect("from_json");
    assert_eq!(
        decoded, conversation,
        "deserialize must not lose information"
    );
    assert_eq!(
        decoded.to_json().expect("re-serialize"),
        json,
        "serialization must be byte-stable"
    );

    // Import into a fresh, identically configured engine.
    let store = Arc::new(InMemoryStore::new());
    let engine = HarnessBuilder::new()
        .store(store)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(world.model.clone() as Arc<dyn Model>)
        .build()
        .expect("engine");
    let imported = engine.import(decoded).await.expect("import");
    assert_eq!(imported.id, session.id, "identity must survive import");

    let reexported = engine.export(imported.id).await.expect("re-export");
    assert_eq!(
        reexported, conversation,
        "export after import must reproduce the conversation exactly"
    );
}
