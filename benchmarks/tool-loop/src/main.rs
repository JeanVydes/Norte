//! What a tool loop actually costs, and what widens it.
//!
//! A tool turn is not one request. The model asks for a tool, the engine runs
//! it, and the model is asked again with the result — the loop below sees as
//! many provider requests as the conversation takes, each one carrying the whole
//! transcript plus the declared tool schemas. This benchmark pins the ask and
//! varies the two things a host controls: **how many tools are declared** and
//! **how big the transcript already is**. Everything the provider reports is
//! recorded per request — bytes, tokens, cache reads, money — so the cost of a
//! wider schema or a longer history is a number.
//!
//! The tool is a real one (`word_count`); the filler tools exist only to widen
//! the declared surface, and they are never called. That is the point: a tool
//! that is declared but unused is still sent on every request.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
//!   cargo run -p norte-bench-tool-loop -- --rounds 1
//! ```

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run, captured as new_capture};
use norte_core::{
    Capabilities, EngineBuilder, FnExecutor, InMemoryBlobStore, InMemoryStore, Message, Modality,
    Part, Role, StreamingGranularity, TextPart, ToolKind, ToolRegistry, Usage,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const PROMPT: &str = "Count the words in this exact text: one two three four. \
                      You must call the word_count tool with that text.";

const USAGE: &str = "\
norte-bench-tool-loop — the cost of a tool loop, live

USAGE:
    norte-bench-tool-loop [OPTIONS]

OPTIONS:
    --rounds <n>    rounds per configuration (default 1)
    --prior <n>     prior Q&A turns to seed (default 3)
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

The matrix crosses declared tools (1 vs 5) with transcript size (0 vs --prior
turns). Live: needs a key and a route in the environment.
  OPENROUTER_API_KEY (or BENCH_KEY_ENV), BENCH_ENDPOINT, BENCH_MODEL, BENCH_PROVIDER,
  BENCH_PAUSE_MS.
";

// ── the tools ───────────────────────────────────────────────────────────────

#[norte_core::tool(name = "word_count", description = "Count the words in a text")]
struct WordCount;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WordCountArgs {
    text: String,
}

#[derive(Debug, Serialize)]
struct WordCountResult {
    words: usize,
}

#[async_trait::async_trait]
impl FnExecutor<WordCountArgs, WordCountResult> for WordCount {
    async fn call(&self, args: WordCountArgs) -> norte_core::Result<WordCountResult> {
        Ok(WordCountResult {
            words: args.text.split_whitespace().count(),
        })
    }
}

/// The declared-but-unused surface: same shape, four distinct names.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FillerArgs {
    note: String,
}

#[derive(Debug, Serialize)]
struct FillerResult {
    ok: bool,
}

macro_rules! filler {
    ($name:ident, $tool:literal, $description:literal) => {
        #[norte_core::tool(name = $tool, description = $description)]
        struct $name;

        #[async_trait::async_trait]
        impl FnExecutor<FillerArgs, FillerResult> for $name {
            async fn call(&self, _args: FillerArgs) -> norte_core::Result<FillerResult> {
                Ok(FillerResult { ok: true })
            }
        }
    };
}

filler!(
    EchoAlpha,
    "echo_alpha",
    "Return the note unchanged. Declared to widen the schema surface."
);
filler!(
    EchoBeta,
    "echo_beta",
    "Return the note unchanged. Declared to widen the schema surface."
);
filler!(
    EchoGamma,
    "echo_gamma",
    "Return the note unchanged. Declared to widen the schema surface."
);
filler!(
    EchoDelta,
    "echo_delta",
    "Return the note unchanged. Declared to widen the schema surface."
);

fn toolset(extras: bool) -> Result<ToolRegistry, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&WordCount)?;
    if extras {
        tools.register_declarator(&EchoAlpha)?;
        tools.register_declarator(&EchoBeta)?;
        tools.register_declarator(&EchoGamma)?;
        tools.register_declarator(&EchoDelta)?;
    }
    Ok(tools)
}

// ── the measurement ─────────────────────────────────────────────────────────

/// One point of the matrix.
struct Config {
    extras: bool,
    prior: usize,
}

fn configs(prior: u64) -> Vec<Config> {
    vec![
        Config {
            extras: false,
            prior: 0,
        },
        Config {
            extras: true,
            prior: 0,
        },
        Config {
            extras: false,
            prior: usize::try_from(prior).unwrap_or(0),
        },
        Config {
            extras: true,
            prior: usize::try_from(prior).unwrap_or(0),
        },
    ]
}

/// One provider request, as the provider reported it.
#[derive(Serialize, Clone)]
struct RequestRow {
    config: String,
    round: u64,
    index: usize,
    messages: usize,
    bytes: usize,
    digest: String,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cost: Option<f64>,
}

/// The summary of one configuration over its rounds.
#[derive(Serialize)]
struct ConfigRecord {
    config: String,
    declared_tools: usize,
    prior_turns: usize,
    rounds: u64,
    schema_bytes: usize,
    requests: u64,
    tool_calls: u64,
    tool_errors: u64,
    first_request_bytes: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cost: f64,
    median_micros: u64,
    requests_detail: Vec<RequestRow>,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "tool-loop",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let rounds = run.cli().option_u64("rounds")?.unwrap_or(1).max(1);
    let prior = run.cli().option_u64("prior")?.unwrap_or(3);
    let route = run
        .route()
        .ok_or("the tool-loop benchmark is live and needs a route")?
        .clone();
    let options = route.turn_options(512);

    let mut records = Vec::new();
    for config in configs(prior) {
        let record = run_config(&route, &config, options.clone(), rounds).await;
        run.record("config", &record);
        records.push(record);
    }

    let report = render(&route, &records, rounds);
    run.table("configs", &SUMMARY_HEADERS, summary_rows(&records));
    run.table("requests", &REQUEST_HEADERS, request_rows(&records));
    run.finish(report)?;
    Ok(())
}

async fn run_config(
    route: &norte_bench_support::Route,
    config: &Config,
    options: norte_core::TurnOptions,
    rounds: u64,
) -> ConfigRecord {
    let mut micros = Vec::new();
    let mut detail = Vec::new();
    let mut schema_bytes = 0_usize;
    let mut first_request_bytes = 0_usize;
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut cache_read_tokens = 0_u64;
    let mut cost = 0.0_f64;
    let mut requests = 0_u64;
    let mut tool_calls = 0_u64;
    let mut tool_errors = 0_u64;
    let mut error = None;

    for round in 0..rounds {
        match one_round(route, config, options.clone(), round).await {
            Ok(round_result) => {
                micros.push(round_result.micros);
                if round == 0 {
                    schema_bytes = round_result.schema_bytes;
                    first_request_bytes = round_result.first_request_bytes;
                }
                input_tokens += round_result.input_tokens;
                output_tokens += round_result.output_tokens;
                cache_read_tokens += round_result.cache_read_tokens;
                cost += round_result.cost;
                requests += round_result.requests;
                tool_calls += round_result.tool_calls;
                tool_errors += round_result.tool_errors;
                detail.extend(round_result.detail);
            }
            Err(message) => error = Some(message),
        }
    }
    micros.sort_unstable();

    ConfigRecord {
        config: format!(
            "tools={} prior={}",
            if config.extras { 5 } else { 1 },
            config.prior
        ),
        declared_tools: if config.extras { 5 } else { 1 },
        prior_turns: config.prior,
        rounds,
        schema_bytes,
        requests,
        tool_calls,
        tool_errors,
        first_request_bytes,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cost,
        median_micros: micros.get(micros.len() / 2).copied().unwrap_or(0),
        requests_detail: detail,
        error,
    }
}

struct RoundResult {
    micros: u64,
    schema_bytes: usize,
    first_request_bytes: usize,
    requests: u64,
    tool_calls: u64,
    tool_errors: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cost: f64,
    detail: Vec<RequestRow>,
}

async fn one_round(
    route: &norte_bench_support::Route,
    config: &Config,
    options: norte_core::TurnOptions,
    round: u64,
) -> Result<RoundResult, String> {
    let captured = new_capture();
    let adapter = route
        .openai(text_capabilities())
        .map_err(|e| e.to_string())?;
    let probe =
        norte_bench_support::RecordingOpenAi::new(adapter, Arc::clone(&captured), route.pause());
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(probe))
        .tools(toolset(config.extras).map_err(|e| e.to_string())?)
        .build()
        .map_err(|e| e.to_string())?;

    let session = engine.create().await.map_err(|e| e.to_string())?;
    if config.prior > 0 {
        engine
            .append_history(session.id, prior_turns(config.prior))
            .await
            .map_err(|e| e.to_string())?;
    }

    let started = Instant::now();
    engine
        .turn(session.id)
        .input(Message::user(PROMPT))
        .options(options)
        .start()
        .finish()
        .await
        .map_err(|e| e.to_string())?;
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

    let history = engine
        .history(session.id)
        .await
        .map_err(|e| e.to_string())?;
    let (tool_calls, tool_errors) = count_tools(&history);
    let usages: Vec<Usage> = history
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .filter_map(|message| message.origin.as_ref())
        .map(|origin| origin.usage.clone())
        .collect();

    let records = captured
        .lock()
        .expect("the probe lock is never held across an await")
        .clone();
    let schema_bytes = records
        .first()
        .and_then(|record| record.body.get("tools"))
        .map_or(0, |tools| tools.to_string().len());
    let first_request_bytes = records.first().map_or(0, |record| record.bytes);
    let detail: Vec<RequestRow> = records
        .iter()
        .map(|record| {
            let usage = usages
                .get(record.index.saturating_sub(1))
                .cloned()
                .unwrap_or_default();
            RequestRow {
                config: format!(
                    "tools={} prior={}",
                    if config.extras { 5 } else { 1 },
                    config.prior
                ),
                round,
                index: record.index,
                messages: record.messages,
                bytes: record.bytes,
                digest: record.digest.chars().take(16).collect(),
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_read: usage.cache_read_tokens,
                cost: norte_bench_support::cost_of(&usage),
            }
        })
        .collect();
    let mut totals = norte_bench_support::Counters::default();
    for usage in &usages {
        totals.add(usage);
    }

    Ok(RoundResult {
        micros,
        schema_bytes,
        first_request_bytes,
        requests: u64::try_from(records.len()).unwrap_or(0),
        tool_calls,
        tool_errors,
        input_tokens: totals.input,
        output_tokens: totals.output,
        cache_read_tokens: totals.cache_read,
        cost: totals.cost,
        detail,
    })
}

/// Prior question-and-answer turns, so the transcript the tool turn carries is
/// longer than the tool ask itself.
fn prior_turns(count: usize) -> Vec<Message> {
    let mut messages = Vec::new();
    for index in 0..count {
        messages.push(Message::user(format!(
            "Question {index}: name one thing about caches."
        )));
        messages.push(Message::new(
            Role::Assistant,
            vec![Part::Text(TextPart::new(format!(
                "Answer {index}: they turn a repeat into a read."
            )))],
        ));
    }
    messages
}

fn count_tools(history: &[Message]) -> (u64, u64) {
    let mut calls = 0_u64;
    let mut errors = 0_u64;
    for message in history {
        for part in &message.parts {
            match part {
                Part::ToolCall(_) => calls += 1,
                Part::ToolResult(result) if result.is_error => errors += 1,
                _ => {}
            }
        }
    }
    (calls, errors)
}

fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

const SUMMARY_HEADERS: [&str; 13] = [
    "config",
    "declared_tools",
    "prior_turns",
    "schema_bytes",
    "first_request_bytes",
    "requests",
    "tool_calls",
    "tool_errors",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cost",
    "median_micros",
];

fn summary_rows(records: &[ConfigRecord]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|record| {
            vec![
                record.config.clone(),
                record.declared_tools.to_string(),
                record.prior_turns.to_string(),
                record.schema_bytes.to_string(),
                record.first_request_bytes.to_string(),
                record.requests.to_string(),
                record.tool_calls.to_string(),
                record.tool_errors.to_string(),
                record.input_tokens.to_string(),
                record.output_tokens.to_string(),
                record.cache_read_tokens.to_string(),
                format!("{:.6}", record.cost),
                record.median_micros.to_string(),
            ]
        })
        .collect()
}

const REQUEST_HEADERS: [&str; 11] = [
    "config",
    "round",
    "index",
    "messages",
    "bytes",
    "digest",
    "input",
    "output",
    "cache_read",
    "cost",
    "note",
];

fn request_rows(records: &[ConfigRecord]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for record in records {
        for request in &record.requests_detail {
            rows.push(vec![
                request.config.clone(),
                request.round.to_string(),
                request.index.to_string(),
                request.messages.to_string(),
                request.bytes.to_string(),
                request.digest.clone(),
                request
                    .input
                    .map_or_else(String::new, |value| value.to_string()),
                request
                    .output
                    .map_or_else(String::new, |value| value.to_string()),
                request
                    .cache_read
                    .map_or_else(String::new, |value| value.to_string()),
                request
                    .cost
                    .map_or_else(String::new, |value| format!("{value:.6}")),
                if request.index == 1 {
                    "carries the declared tool schemas".to_owned()
                } else {
                    "carries the tool result".to_owned()
                },
            ]);
        }
    }
    rows
}

#[allow(clippy::too_many_lines)]
fn render(route: &norte_bench_support::Route, records: &[ConfigRecord], rounds: u64) -> Report {
    let mut report = Report::new("Tool loop");
    report.paragraph(
        "One tool ask, over a matrix of how many tools are declared and how long \
         the transcript already is. A tool turn is not one request: the loop asks, \
         runs, and asks again with the result, and every request carries the whole \
         transcript plus every declared schema — including the tools never called.",
    );
    report.section("Run");
    report.kv(&[
        ("route", route.summary()),
        (
            "pause",
            format!(
                "{} ms before each request; the elapsed column includes it",
                route.pause_ms
            ),
        ),
        ("rounds per configuration", rounds.to_string()),
        ("ask", PROMPT.to_owned()),
    ]);

    report.section("Configuration");
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            vec![
                record.config.clone(),
                record.schema_bytes.to_string(),
                record.first_request_bytes.to_string(),
                record.requests.to_string(),
                record.tool_calls.to_string(),
                record.tool_errors.to_string(),
                record.input_tokens.to_string(),
                record.output_tokens.to_string(),
                record.cache_read_tokens.to_string(),
                format!("{:.6}", record.cost),
                record.median_micros.to_string(),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    report.table(
        &[
            "config",
            "schema bytes",
            "first request bytes",
            "requests",
            "tool calls",
            "tool errors",
            "input tokens",
            "output tokens",
            "cache read",
            "cost",
            "median µs",
            "error",
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
            Align::Right,
            Align::Right,
            Align::Left,
        ],
        &rows,
    );
    report.bars(
        &records
            .iter()
            .map(|record| {
                (
                    record.config.clone(),
                    u64::try_from(record.declared_tools).unwrap_or(0),
                )
            })
            .collect::<Vec<_>>(),
        16,
    );

    report.section("Requests");
    report.paragraph(
        "Every provider request the loop made, with the usage the provider reported \
         for it. The first carries the schemas; each later one carries the tool result \
         and the whole prefix again.",
    );
    let rows: Vec<Vec<String>> = records
        .iter()
        .flat_map(|record| {
            record.requests_detail.iter().map(|request| {
                vec![
                    request.config.clone(),
                    request.index.to_string(),
                    request.messages.to_string(),
                    request.bytes.to_string(),
                    request.digest.clone(),
                    request
                        .input
                        .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    request
                        .output
                        .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    request
                        .cache_read
                        .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    request
                        .cost
                        .map_or_else(|| "—".to_owned(), |value| format!("{value:.6}")),
                ]
            })
        })
        .collect();
    report.table(
        &[
            "config",
            "request",
            "messages",
            "bytes",
            "digest",
            "input",
            "output",
            "cache read",
            "cost",
        ],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
        &rows,
    );

    report.section("Reading");
    let widest = records
        .iter()
        .map(|record| record.schema_bytes)
        .max()
        .unwrap_or(0);
    let narrowest = records
        .iter()
        .map(|record| record.schema_bytes)
        .min()
        .unwrap_or(0);
    report.paragraph(&format!(
        "The declared schemas alone are {widest} bytes at the widest and {narrowest} at the \
         narrowest, and those bytes are re-sent on every request of the loop — not once per \
         turn. Read the `bytes` column per request beside `input tokens`: that is the schema \
         and the transcript being paid for again each time the model is asked."
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_crosses_tools_and_transcript() {
        let configs = configs(3);
        assert_eq!(configs.len(), 4);
        assert!(configs.iter().any(|config| config.extras));
        assert!(configs.iter().any(|config| config.prior == 3));
    }

    #[test]
    fn prior_turns_double_up() {
        assert_eq!(prior_turns(3).len(), 6);
    }

    #[test]
    fn the_toolsets_differ() {
        let narrow = toolset(false).expect("tools");
        let wide = toolset(true).expect("tools");
        assert!(narrow.len() < wide.len());
    }
}
