//! Export/import fidelity, at scale: many generated sessions, four wires, and
//! a byte-for-byte comparison at every seam a host would cross.
//!
//! The guarantee is the one that costs money when it breaks. A conversation is
//! exported, repacked, re-imported into a fresh engine, re-exported, and then
//! both engines reconstruct the same next turn. If any of those bytes differ,
//! the provider sees a prefix it never cached, and the cache — the whole point
//! of an append-only reconstruction (FR-RECON-1) — is gone.
//!
//! Four seams are compared for every session, on four wires:
//!
//! 1. **Envelope** — `export -> to_json -> from_json -> to_json` is byte-stable.
//! 2. **Re-export** — `export -> import -> export` is byte-identical to the
//!    first export. The session id travels, so the cache key does too.
//! 3. **Reconstructed request** — the next turn's [`ModelRequest`], after the
//!    import, serialises to the same bytes as the original's.
//! 4. **Provider body** — the wire's own projection of that request is
//!    identical, byte for byte, and the exact `sha256` of both is recorded.
//!
//! What is generated is deliberately varied, and deliberately seeded: turns
//! with plain answers, turns with a tool call and its result, an ephemeral host
//! message (a cache class that must reorder to the end), and random lengths, so
//! the comparison is not one happy path repeated. Same seed, same sessions,
//! every machine.
//!
//! ```sh
//! cargo run -p norte-bench-roundtrip-fidelity -- --cases 200 --seed 1
//! ```
//!
//! Offline: no provider is called and no key is needed. It runs in CI.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run};
use norte_core::{
    CacheClass, CallId, Conversation, Message, Model, ModelRef, ModelRequest, Part, Role, TextPart,
    ToolCallPart, ToolResultPart,
};
use norte_harness::{Harness, HarnessBuilder};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ProbeModel, text_capabilities};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

const USAGE: &str = "\
norte-bench-roundtrip-fidelity — export/import byte-identity at scale

USAGE:
    norte-bench-roundtrip-fidelity [OPTIONS]

OPTIONS:
    --cases <n>    sessions generated per wire (default 200)
    --seed <n>     generator seed (default 1)
    --report <p>   write the markdown report to <p>
    --jsonl <p>    write the JSONL trail to <p>
    --help, -h     print this help

Offline: no provider is called. Four wires are exercised:
openai-responses, openai-chat, anthropic-messages, gemini-generate.
";

/// One wire, its adapter, and how to project a reconstructed request onto the
/// exact body that wire would send.
struct Wire {
    name: &'static str,
    adapter: Arc<dyn Model>,
    body_of: BodyOf,
}

/// The projection from a reconstructed request to a wire's provider body.
type BodyOf = Box<dyn Fn(&ModelRequest) -> norte_core::Result<Value>>;

/// The record of one session: every seam, its verdict, and both digests.
///
/// Four verdicts is four seams, one per column of the report; a state machine
/// would hide which seam failed, which is the whole point.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize)]
struct CaseRecord {
    wire: String,
    case: u64,
    messages: usize,
    envelope_bytes: usize,
    json_identical: bool,
    reexport_identical: bool,
    request_identical: bool,
    body_identical: bool,
    envelope_digest: String,
    restored_digest: String,
    micros: u64,
    error: Option<String>,
}

impl CaseRecord {
    /// The one predicate that says the session crossed every seam unchanged.
    fn passed(&self) -> bool {
        self.error.is_none()
            && self.json_identical
            && self.reexport_identical
            && self.request_identical
            && self.body_identical
    }
}

/// A tiny deterministic generator: same seed, same sessions, every machine.
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

    fn bounded(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn word(&mut self) -> String {
        let len = usize::try_from(2 + self.bounded(8)).unwrap_or(2);
        (0..len)
            .map(|_| char::from(b'a' + u8::try_from(self.next() % 26).unwrap_or(0)))
            .collect()
    }

    fn sentence(&mut self, words: u64) -> String {
        let mut text = String::new();
        for _ in 0..words {
            text.push_str(&self.word());
            text.push(' ');
        }
        text.trim().to_owned()
    }

    /// A deterministic call id, so the whole session — ids included — is
    /// reproducible from the seed. `CallId::new` would be random.
    fn call_id(&mut self) -> CallId {
        let mut bytes = [0_u8; 16];
        for chunk in bytes.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_be_bytes());
        }
        CallId::from(Uuid::from_bytes(bytes))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "roundtrip-fidelity",
        USAGE,
        Mode::Offline,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let cases = run.cli().option_u64("cases")?.unwrap_or(200);
    let seed = run.cli().option_u64("seed")?.unwrap_or(1);
    let wires = wires()?;

    let mut rng = Rng::new(seed);
    let mut records: Vec<CaseRecord> = Vec::new();
    for wire in &wires {
        for case in 0..cases {
            let messages = generate(&mut rng, case);
            let record = measure(wire, case, messages).await;
            run.record("case", &record);
            records.push(record);
        }
    }

    let report = render(&wires, &records, seed, cases);
    run.table("cases", &CSV_HEADERS, csv_rows(&records));
    run.finish(report)?;
    Ok(())
}

/// Builds the four wires, each with its own body projection.
///
/// # Errors
///
/// Returns an error when a preset cannot be built (an invalid model id or a
/// default `HTTP` client that will not construct).
fn wires() -> Result<Vec<Wire>, Box<dyn Error>> {
    let caps = text_capabilities();
    let mut wires = Vec::new();

    let adapter = norte_openai::presets::openai("gpt-5", "sk-test", caps.clone())?;
    let body_of = {
        let adapter = adapter.clone();
        Box::new(move |request: &ModelRequest| adapter.request_body(request))
    };
    wires.push(Wire {
        name: "openai-responses",
        adapter: Arc::new(adapter),
        body_of,
    });

    let profile = norte_openai::presets::openai_profile("sk-test")?;
    let reference = ModelRef::new(profile.provider.clone(), "gpt-5".parse()?);
    let adapter = norte_openai::OpenAiChat::new(reference, profile, caps.clone())?;
    let body_of = {
        let adapter = adapter.clone();
        Box::new(move |request: &ModelRequest| adapter.request_body(request))
    };
    wires.push(Wire {
        name: "openai-chat",
        adapter: Arc::new(adapter),
        body_of,
    });

    let adapter = norte_anthropic::presets::anthropic("claude-sonnet-4", "sk-test", caps.clone())?;
    let body_of = {
        let adapter = adapter.clone();
        Box::new(move |request: &ModelRequest| adapter.request_body(request))
    };
    wires.push(Wire {
        name: "anthropic-messages",
        adapter: Arc::new(adapter),
        body_of,
    });

    let adapter = norte_gemini::presets::gemini("gemini-2.5-pro", "sk-test", caps)?;
    let body_of = {
        let adapter = adapter.clone();
        Box::new(move |request: &ModelRequest| adapter.request_body(request))
    };
    wires.push(Wire {
        name: "gemini-generate",
        adapter: Arc::new(adapter),
        body_of,
    });

    Ok(wires)
}

/// One generated session: a system prompt, plain turns, tool rounds and
/// sometimes an ephemeral host message.
fn generate(rng: &mut Rng, _case: u64) -> Vec<Message> {
    let mut messages = vec![Message::system("You are terse and exact.")];
    let turns = 1 + rng.bounded(3);
    for turn in 0..turns {
        let question_words = 2 + rng.bounded(8);
        let question = rng.sentence(question_words);
        messages.push(Message::user(question));
        if turn % 2 == 0 {
            let answer_words = 1 + rng.bounded(10);
            let answer = rng.sentence(answer_words);
            messages.push(Message::new(
                Role::Assistant,
                vec![Part::Text(TextPart::new(answer))],
            ));
        } else {
            let call_id = rng.call_id();
            messages.push(Message::new(
                Role::Assistant,
                vec![
                    Part::Text(TextPart::new("Let me look.")),
                    Part::ToolCall(ToolCallPart {
                        call_id,
                        name: "lookup".to_owned(),
                        arguments: json!({ "query": rng.word() }),
                        continuity: None,
                    }),
                ],
            ));
            let result_words = 1 + rng.bounded(6);
            messages.push(Message::tool(vec![ToolResultPart::text(
                call_id,
                rng.sentence(result_words),
            )]));
        }
    }
    if rng.bounded(2) == 0 {
        messages
            .push(Message::system(format!("run={}", rng.next())).with_cache(CacheClass::Ephemeral));
    }
    messages
}

fn engine(model: Arc<dyn Model>, store: Arc<InMemoryStore>) -> Harness {
    HarnessBuilder::new()
        .store(store)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(model)
        .build()
        .expect("engine")
}

/// Measures one session end to end, turning any error into a recorded failure
/// rather than aborting the run.
async fn measure(wire: &Wire, case: u64, messages: Vec<Message>) -> CaseRecord {
    let count = messages.len();
    match measure_inner(wire, messages).await {
        Ok(mut record) => {
            record.case = case;
            record.messages = count;
            record
        }
        Err(error) => CaseRecord {
            wire: wire.name.to_owned(),
            case,
            messages: count,
            envelope_bytes: 0,
            json_identical: false,
            reexport_identical: false,
            request_identical: false,
            body_identical: false,
            envelope_digest: String::new(),
            restored_digest: String::new(),
            micros: 0,
            error: Some(error),
        },
    }
}

async fn measure_inner(wire: &Wire, messages: Vec<Message>) -> Result<CaseRecord, String> {
    let started = Instant::now();

    let probe_a = Arc::new(ProbeModel::new(Arc::clone(&wire.adapter)));
    let engine_a = engine(
        probe_a.clone() as Arc<dyn Model>,
        Arc::new(InMemoryStore::new()),
    );
    let session = engine_a.create().await.map_err(|e| e.to_string())?;
    engine_a
        .append_history(session.id, messages)
        .await
        .map_err(|e| e.to_string())?;
    let conversation = engine_a
        .export(session.id)
        .await
        .map_err(|e| e.to_string())?;
    let json = conversation.to_json().map_err(|e| e.to_string())?;
    let repacked = Conversation::from_json(&json)
        .map_err(|e| e.to_string())?
        .to_json()
        .map_err(|e| e.to_string())?;
    let json_identical = json == repacked;

    let probe_b = Arc::new(ProbeModel::new(Arc::clone(&wire.adapter)));
    let engine_b = engine(
        probe_b.clone() as Arc<dyn Model>,
        Arc::new(InMemoryStore::new()),
    );
    let imported = engine_b
        .import(Conversation::from_json(&json).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;
    let restored = engine_b
        .export(imported.id)
        .await
        .map_err(|e| e.to_string())?
        .to_json()
        .map_err(|e| e.to_string())?;
    let reexport_identical = json == restored;

    engine_a
        .send(session.id, Message::user("and now?"))
        .finish()
        .await
        .map_err(|e| e.to_string())?;
    engine_b
        .send(imported.id, Message::user("and now?"))
        .finish()
        .await
        .map_err(|e| e.to_string())?;

    let request_a = probe_a
        .last_request()
        .ok_or_else(|| "engine a reconstructed no request".to_owned())?;
    let request_b = probe_b
        .last_request()
        .ok_or_else(|| "engine b reconstructed no request".to_owned())?;
    let request_identical = bytes(&request_a)? == bytes(&request_b)?;

    let body_a = (wire.body_of)(&request_a).map_err(|e| e.to_string())?;
    let body_b = (wire.body_of)(&request_b).map_err(|e| e.to_string())?;
    let body_identical = serde_json::to_string(&body_a).map_err(|e| e.to_string())?
        == serde_json::to_string(&body_b).map_err(|e| e.to_string())?;

    Ok(CaseRecord {
        wire: wire.name.to_owned(),
        case: 0,
        messages: 0,
        envelope_bytes: json.len(),
        json_identical,
        reexport_identical,
        request_identical,
        body_identical,
        envelope_digest: digest(json.as_bytes()),
        restored_digest: digest(restored.as_bytes()),
        micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        error: None,
    })
}

fn bytes(request: &ModelRequest) -> Result<String, String> {
    serde_json::to_string(request).map_err(|e| e.to_string())
}

fn digest(text: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text);
    format!("{:x}", hasher.finalize())
}

const CSV_HEADERS: [&str; 12] = [
    "wire",
    "case",
    "messages",
    "envelope_bytes",
    "json_identical",
    "reexport_identical",
    "request_identical",
    "body_identical",
    "envelope_digest",
    "restored_digest",
    "micros",
    "error",
];

fn csv_rows(records: &[CaseRecord]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|record| {
            vec![
                record.wire.clone(),
                record.case.to_string(),
                record.messages.to_string(),
                record.envelope_bytes.to_string(),
                record.json_identical.to_string(),
                record.reexport_identical.to_string(),
                record.request_identical.to_string(),
                record.body_identical.to_string(),
                record.envelope_digest.clone(),
                record.restored_digest.clone(),
                record.micros.to_string(),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

/// The totals for one wire.
#[derive(Default)]
struct Aggregate {
    cases: u64,
    passed: u64,
    bytes: u64,
    micros: u64,
    json_ok: u64,
    request_ok: u64,
    body_ok: u64,
}

#[allow(clippy::too_many_lines)]
fn render(wires: &[Wire], records: &[CaseRecord], seed: u64, cases: u64) -> Report {
    let mut report = Report::new("Export/import fidelity");
    report.paragraph(
        "Every session is exported, repacked, imported into a fresh engine and re-exported; \
         both engines then reconstruct the same next turn. Every seam below must be \
         byte-for-byte identical, or the provider cache key moves and a cache miss is billed \
         (FR-EXP-1, FR-EXP-2, FR-EXP-6, FR-RECON-12).",
    );
    report.section("Run");
    report.kv(&[
        ("mode", "offline (deterministic, no provider)".to_owned()),
        ("wires", wires.len().to_string()),
        ("cases per wire", cases.to_string()),
        ("sessions total", records.len().to_string()),
        ("generator seed", seed.to_string()),
    ]);

    report.section("Per wire");
    let mut aggregates: Vec<(&str, Aggregate)> = Vec::new();
    for wire in wires {
        let mut aggregate = Aggregate::default();
        for record in records.iter().filter(|record| record.wire == wire.name) {
            aggregate.cases += 1;
            aggregate.passed += u64::from(record.passed());
            aggregate.bytes += u64::try_from(record.envelope_bytes).unwrap_or(0);
            aggregate.micros += record.micros;
            aggregate.json_ok += u64::from(record.json_identical);
            aggregate.request_ok += u64::from(record.request_identical);
            aggregate.body_ok += u64::from(record.body_identical);
        }
        aggregates.push((wire.name, aggregate));
    }
    let rows: Vec<Vec<String>> = aggregates
        .iter()
        .map(|(name, a)| {
            let average_bytes = a.bytes.checked_div(a.cases).unwrap_or(0);
            let average_micros = a.micros.checked_div(a.cases).unwrap_or(0);
            vec![
                (*name).to_owned(),
                a.cases.to_string(),
                a.passed.to_string(),
                (a.cases - a.passed).to_string(),
                average_bytes.to_string(),
                format!("{}/{}", a.json_ok, a.cases),
                format!("{}/{}", a.request_ok, a.cases),
                format!("{}/{}", a.body_ok, a.cases),
                average_micros.to_string(),
            ]
        })
        .collect();
    report.table(
        &[
            "wire",
            "cases",
            "passed",
            "failed",
            "avg bytes",
            "envelope",
            "request",
            "body",
            "avg µs",
        ],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
        &rows,
    );
    report.bars(
        &aggregates
            .iter()
            .map(|(name, a)| ((*name).to_owned(), a.passed))
            .collect::<Vec<_>>(),
        32,
    );

    let failures: Vec<&CaseRecord> = records.iter().filter(|record| !record.passed()).collect();
    report.section("Failures");
    if failures.is_empty() {
        report.paragraph("None. Every seam held on every session.");
    } else {
        let rows: Vec<Vec<String>> = failures
            .iter()
            .take(20)
            .map(|record| {
                vec![
                    record.wire.clone(),
                    record.case.to_string(),
                    verdict(record.json_identical),
                    verdict(record.reexport_identical),
                    verdict(record.request_identical),
                    verdict(record.body_identical),
                    record
                        .error
                        .clone()
                        .unwrap_or_else(|| "a byte differed".to_owned()),
                ]
            })
            .collect();
        report.table(
            &[
                "wire",
                "case",
                "envelope",
                "re-export",
                "request",
                "body",
                "detail",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Left,
            ],
            &rows,
        );
    }
    report.section("Artifacts");
    report.paragraph(
        "Beside this report: a JSONL trail, one record per session with both `sha256` digests, \
         and a CSV of the same table for a spreadsheet.",
    );
    report
}

fn verdict(ok: bool) -> String {
    if ok {
        "ok".to_owned()
    } else {
        "DIFFERS".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_wire_round_trips_a_handful_of_sessions() {
        let wires = wires().expect("wires");
        let mut rng = Rng::new(7);
        for wire in &wires {
            for case in 0..5 {
                let messages = generate(&mut rng, case);
                let record = measure(wire, case, messages).await;
                assert!(record.passed(), "{} case {case}: {record:?}", wire.name);
            }
        }
    }

    #[test]
    fn the_generator_is_deterministic_for_a_seed() {
        let mut left = Rng::new(1);
        let mut right = Rng::new(1);
        for _ in 0..50 {
            assert_eq!(
                generate(&mut left, 0),
                generate(&mut right, 0),
                "same seed must produce the same session"
            );
        }
    }
}
