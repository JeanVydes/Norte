//! The provider's counters against the engine's accounting: does every request
//! that reached the provider leave exactly one usage record, and does that
//! record keep everything the provider sent?
//!
//! A host budgets from the numbers the engine commits, so a lost or duplicated
//! usage record is a budget that is wrong. This benchmark runs a multi-turn
//! conversation, then reconciles three views:
//!
//! 1. **The requests** — counted by a recording model, one per call the engine
//!    actually made.
//! 2. **The engine's records** — one per committed assistant message, the usage
//!    the engine normalized out of the provider's payload.
//! 3. **The provider's payload** — the `vendor` map the engine keeps verbatim,
//!    listed key by key so a counter the engine did not model is visible rather
//!    than lost.
//!
//! The reconciliation is a verdict, not a vibe: `one record per request` either
//! holds or it does not, and every field the provider sent is shown.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-bench-token-counting -- --turns 3
//! ```

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run, captured as new_capture};
use norte_core::{Capabilities, Message, Modality, Role, StreamingGranularity};
use norte_harness::InMemoryStore;
use norte_harness::{HarnessBuilder, InMemoryBlobStore};
use serde::Serialize;

const USAGE: &str = "\
norte-bench-token-counting — provider counters against the engine's accounting, live

USAGE:
    norte-bench-token-counting [OPTIONS]

OPTIONS:
    --turns <n>     user turns in the conversation (default 3)
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

Live: needs a key and a route in the environment.
  OPENROUTER_API_KEY (or BENCH_KEY_ENV), BENCH_ENDPOINT, BENCH_MODEL, BENCH_PROVIDER,
  BENCH_PAUSE_MS.
";

const PROMPTS: [&str; 6] = [
    "In one sentence: what is a provider cache?",
    "In one sentence: why does a cache miss cost money?",
    "In one sentence: what is a prefix?",
    "In one sentence: what is a token?",
    "In one sentence: what is streaming?",
    "In one sentence: what is a tool call?",
];

/// A long, fixed system message: the prefix has to clear the provider's caching
/// floor before the cache counters in the reconciliation mean anything. The
/// rules are deliberately dull; they exist to be re-read, not to be answered.
fn system() -> String {
    let mut text = String::from(
        "You are a concise assistant. Answer the user in one sentence, in plain text. The rules \
         below are part of the session and never change.\n\n",
    );
    for index in 0..64 {
        let _ = writeln!(
            text,
            "Rule {index:02}: keep the ledger append-only; name every request; never rewrite a \
             committed message; the whole prefix is re-read on every turn."
        );
    }
    text
}

/// One usage record, with the vendor's own payload beside it.
#[derive(Serialize, Clone)]
struct UsageRecord {
    index: usize,
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    derived_total: u64,
    vendor_keys: Vec<String>,
    cost: Option<f64>,
    complete: bool,
}

/// The reconciliation of the whole conversation.
#[derive(Serialize)]
struct Verdict {
    turns: u64,
    requests: u64,
    usage_records: u64,
    complete_records: u64,
    one_record_per_request: bool,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
    cost: f64,
    unpriced: u64,
    vendor_only_keys: Vec<String>,
    records: Vec<UsageRecord>,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "token-counting",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let turns = run.cli().option_u64("turns")?.unwrap_or(3).max(1);
    let route = run
        .route()
        .ok_or("the token-counting benchmark is live and needs a route")?
        .clone();
    let options = route.turn_options(256);

    let verdict = measure(&route, turns, options).await;
    run.record("verdict", &verdict);

    let report = render(&route, &verdict);
    run.table("records", &RECORD_HEADERS, record_rows(&verdict));
    run.finish(report)?;
    Ok(())
}

async fn measure(
    route: &norte_bench_support::Route,
    turns: u64,
    options: norte_core::TurnOptions,
) -> Verdict {
    match measure_inner(route, turns, options).await {
        Ok(verdict) => verdict,
        Err(error) => Verdict {
            turns,
            requests: 0,
            usage_records: 0,
            complete_records: 0,
            one_record_per_request: false,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 0,
            cost: 0.0,
            unpriced: 0,
            vendor_only_keys: Vec::new(),
            records: Vec::new(),
            error: Some(error),
        },
    }
}

async fn measure_inner(
    route: &norte_bench_support::Route,
    turns: u64,
    options: norte_core::TurnOptions,
) -> Result<Verdict, String> {
    let captured = new_capture();
    let adapter = route
        .openai(text_capabilities())
        .map_err(|e| e.to_string())?;
    let probe =
        norte_bench_support::RecordingOpenAi::new(adapter, Arc::clone(&captured), route.pause());
    let engine = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(probe))
        .build()
        .map_err(|e| e.to_string())?;
    let session = engine.create().await.map_err(|e| e.to_string())?;
    engine
        .append_history(session.id, vec![Message::system(system())])
        .await
        .map_err(|e| e.to_string())?;

    for turn in 0..turns {
        let prompt = PROMPTS[usize::try_from(turn).unwrap_or(0) % PROMPTS.len()];
        engine
            .turn(session.id)
            .input(Message::user(prompt))
            .options(options.clone())
            .start()
            .finish()
            .await
            .map_err(|e| e.to_string())?;
    }

    let history = engine
        .history(session.id)
        .await
        .map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    let mut totals = norte_bench_support::Counters::default();
    let mut vendor_keys: Vec<String> = Vec::new();
    for message in &history {
        if message.role != Role::Assistant {
            continue;
        }
        let Some(origin) = &message.origin else {
            continue;
        };
        let usage = &origin.usage;
        totals.add(usage);
        let mut keys: Vec<String> = usage.vendor.keys().cloned().collect();
        keys.sort();
        for key in &keys {
            if !vendor_keys.contains(key) {
                vendor_keys.push(key.clone());
            }
        }
        let derived_total = usage.total_tokens();
        records.push(UsageRecord {
            index: records.len() + 1,
            input: usage.input_tokens,
            output: usage.output_tokens,
            reasoning: usage.reasoning_tokens,
            cache_read: usage.cache_read_tokens,
            cache_write: usage.cache_write_tokens,
            derived_total,
            vendor_keys: keys,
            cost: norte_bench_support::cost_of(usage),
            complete: usage.input_tokens.is_some() && usage.output_tokens.is_some(),
        });
    }

    let requests = u64::try_from(
        captured
            .lock()
            .expect("the probe lock is never held across an await")
            .len(),
    )
    .unwrap_or(0);
    let usage_records = u64::try_from(records.len()).unwrap_or(0);
    let complete_records = records.iter().filter(|record| record.complete).count() as u64;
    vendor_keys.sort();

    Ok(Verdict {
        turns,
        requests,
        usage_records,
        complete_records,
        one_record_per_request: requests == usage_records,
        input_tokens: totals.input,
        output_tokens: totals.output,
        reasoning_tokens: totals.reasoning,
        cache_read_tokens: totals.cache_read,
        cache_write_tokens: totals.cache_write,
        total_tokens: totals.total_tokens(),
        cost: totals.cost,
        unpriced: totals.unpriced,
        vendor_only_keys: vendor_keys,
        records,
        error: None,
    })
}

fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}

const RECORD_HEADERS: [&str; 10] = [
    "index",
    "input",
    "output",
    "reasoning",
    "cache_read",
    "cache_write",
    "derived_total",
    "cost",
    "complete",
    "vendor_keys",
];

fn record_rows(verdict: &Verdict) -> Vec<Vec<String>> {
    verdict
        .records
        .iter()
        .map(|record| {
            vec![
                record.index.to_string(),
                optional(record.input),
                optional(record.output),
                optional(record.reasoning),
                optional(record.cache_read),
                optional(record.cache_write),
                record.derived_total.to_string(),
                record
                    .cost
                    .map_or_else(String::new, |value| format!("{value:.6}")),
                record.complete.to_string(),
                record.vendor_keys.join(" "),
            ]
        })
        .collect()
}

fn optional(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

#[allow(clippy::too_many_lines)]
fn render(route: &norte_bench_support::Route, verdict: &Verdict) -> Report {
    let mut report = Report::new("Token counting");
    report.paragraph(
        "A host budgets from the numbers the engine commits. This run reconciles the requests \
         the engine made, the usage records it committed, and the provider's own payload — which \
         the engine keeps verbatim — so a lost record or a dropped counter is visible.",
    );
    report.section("Run");
    report.kv(&[
        ("route", route.summary()),
        (
            "pause",
            format!("{} ms before each request", route.pause_ms),
        ),
        (
            "system prefix",
            format!("{} bytes, so the cache floor is cleared", system().len()),
        ),
        ("user turns", verdict.turns.to_string()),
        (
            "verdict",
            if verdict.one_record_per_request {
                "one usage record per request".to_owned()
            } else {
                "MISCOUNTED".to_owned()
            },
        ),
    ]);

    report.section("Reconciliation");
    report.kv(&[
        ("provider requests", verdict.requests.to_string()),
        ("usage records", verdict.usage_records.to_string()),
        (
            "complete records (input and output present)",
            verdict.complete_records.to_string(),
        ),
        (
            "one record per request",
            verdict.one_record_per_request.to_string(),
        ),
        ("input tokens", verdict.input_tokens.to_string()),
        ("output tokens", verdict.output_tokens.to_string()),
        ("reasoning tokens", verdict.reasoning_tokens.to_string()),
        ("cache read tokens", verdict.cache_read_tokens.to_string()),
        ("cache write tokens", verdict.cache_write_tokens.to_string()),
        ("derived total tokens", verdict.total_tokens.to_string()),
        ("cost", format!("{:.6}", verdict.cost)),
        ("unpriced records", verdict.unpriced.to_string()),
        (
            "provider keys kept verbatim",
            if verdict.vendor_only_keys.is_empty() {
                "none".to_owned()
            } else {
                verdict.vendor_only_keys.join(", ")
            },
        ),
    ]);

    report.section("Per record");
    let rows: Vec<Vec<String>> = verdict
        .records
        .iter()
        .map(|record| {
            vec![
                record.index.to_string(),
                optional(record.input),
                optional(record.output),
                optional(record.reasoning),
                optional(record.cache_read),
                optional(record.cache_write),
                record.derived_total.to_string(),
                record
                    .cost
                    .map_or_else(|| "—".to_owned(), |value| format!("{value:.6}")),
                record.vendor_keys.join(" "),
            ]
        })
        .collect();
    report.table(
        &[
            "record",
            "input",
            "output",
            "reasoning",
            "cache read",
            "cache write",
            "derived total",
            "cost",
            "provider keys (verbatim)",
        ],
        &[
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
        &rows,
    );

    report.section("Reading");
    if let Some(error) = &verdict.error {
        report.paragraph(&format!("The run failed: {error}"));
    } else if verdict.one_record_per_request && verdict.complete_records == verdict.usage_records {
        report.paragraph(
            "Every request left exactly one complete usage record, and the provider's keys are \
             shown beside the normalized counters, so nothing it sent is hidden. The derived \
             total is input plus output, which is what a budget should be built on.",
        );
    } else {
        report.paragraph(
            "The reconciliation did not hold. Read the table above against the request count: a \
             request with no record, or a record with no input or output, is a number a host \
             would silently budget wrong.",
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompts_cover_the_turns() {
        assert!(PROMPTS.len() >= 3);
    }
}
