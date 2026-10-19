//! How much friction a schema has, as a number: the requests and the rejected
//! calls it takes the model to make one valid call.
//!
//! When a tool's arguments do not satisfy its schema, the engine commits the
//! failure as a tool error and asks the model again — so a schema that is hard
//! to satisfy is paid for in requests, tokens and money, not just in an error
//! message. This benchmark runs three shapes against a deliberately awkward ask
//! and counts what happened: how many calls the model made, how many the schema
//! rejected, and how many requests the loop took.
//!
//! The asks are traps on purpose — an enum value that is not in the enum, a
//! field the schema does not declare. What the model does with a trap is the
//! measurement; whether it always falls for it is not, and a schema that needs
//! zero extra requests is reported as exactly that.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
//!   cargo run -p norte-bench-schema-friction -- --rounds 3
//! ```

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run, captured as new_capture};
use norte_core::{
    Capabilities, EngineBuilder, FnExecutor, InMemoryBlobStore, InMemoryStore, Message, Modality,
    Part, Role, StreamingGranularity, ToolKind, ToolRegistry,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const USAGE: &str = "\
norte-bench-schema-friction — requests until a schema-valid tool call, live

USAGE:
    norte-bench-schema-friction [OPTIONS]

OPTIONS:
    --rounds <n>    rounds per schema (default 3)
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

Three shapes are exercised with a trap ask: a flat string, an enum, and a nested
object with `deny_unknown_fields`. Live: needs a key and a route.
";

// ── the schemas, from easy to awkward ───────────────────────────────────────

#[norte_core::tool(name = "lookup_city", description = "Look up a city by name.")]
struct LookupCity;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CityArgs {
    city: String,
}

#[derive(Debug, Serialize)]
struct CityResult {
    found: bool,
}

#[async_trait::async_trait]
impl FnExecutor<CityArgs, CityResult> for LookupCity {
    async fn call(&self, _args: CityArgs) -> norte_core::Result<CityResult> {
        Ok(CityResult { found: true })
    }
}

#[norte_core::tool(name = "set_priority", description = "Set the priority of a ticket.")]
struct SetPriority;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PriorityArgs {
    priority: Priority,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Priority {
    Low,
    Medium,
    High,
}

#[derive(Debug, Serialize)]
struct PriorityResult {
    set: bool,
}

#[async_trait::async_trait]
impl FnExecutor<PriorityArgs, PriorityResult> for SetPriority {
    async fn call(&self, _args: PriorityArgs) -> norte_core::Result<PriorityResult> {
        Ok(PriorityResult { set: true })
    }
}

#[norte_core::tool(name = "book_room", description = "Book a hotel room for one guest.")]
struct BookRoom;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BookingArgs {
    guest: Guest,
    nights: u32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Guest {
    name: String,
    email: String,
}

#[derive(Debug, Serialize)]
struct BookingResult {
    reference: String,
}

#[async_trait::async_trait]
impl FnExecutor<BookingArgs, BookingResult> for BookRoom {
    async fn call(&self, _args: BookingArgs) -> norte_core::Result<BookingResult> {
        Ok(BookingResult {
            reference: "BK-1".to_owned(),
        })
    }
}

// ── the scenarios ───────────────────────────────────────────────────────────

struct Scenario {
    name: &'static str,
    shape: &'static str,
    trap: &'static str,
    prompt: &'static str,
    build: fn() -> Result<ToolRegistry, Box<dyn Error>>,
}

fn flat_tools() -> Result<ToolRegistry, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&LookupCity)?;
    Ok(tools)
}

fn enum_tools() -> Result<ToolRegistry, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&SetPriority)?;
    Ok(tools)
}

fn nested_tools() -> Result<ToolRegistry, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&BookRoom)?;
    Ok(tools)
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "flat",
            shape: "one string field",
            trap: "none",
            prompt: "Use lookup_city to look up the city Paris. Call the tool now.",
            build: flat_tools,
        },
        Scenario {
            name: "enum",
            shape: "one enum field (low, medium, high)",
            trap: "the ask insists on `urgent`, which is not in the enum",
            prompt: "Set the ticket priority using set_priority. The priority must be the exact \
                     word `urgent`. Call the tool now, even if the schema seems to disagree.",
            build: enum_tools,
        },
        Scenario {
            name: "nested",
            shape: "a nested guest object and a number, deny_unknown_fields",
            trap: "the ask insists on a `floor`, which the schema does not declare",
            prompt: "Book a room with book_room for guest Ana, email ana@example.test, for two \
                     nights. The payload must also include a `floor` field set to 12, exactly as \
                     asked. Call the tool now.",
            build: nested_tools,
        },
    ]
}

// ── the measurement ─────────────────────────────────────────────────────────

#[allow(clippy::struct_field_names)]
#[derive(Serialize, Clone)]
struct Round {
    schema: String,
    round: u64,
    requests: u64,
    calls: u64,
    rejected: u64,
    accepted: u64,
    answered: bool,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
    micros: u64,
}

#[derive(Serialize)]
struct SchemaRecord {
    schema: String,
    shape: String,
    trap: String,
    rounds: u64,
    requests: u64,
    calls: u64,
    rejected: u64,
    accepted: u64,
    answered: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
    median_micros: u64,
    detail: Vec<Round>,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "schema-friction",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let rounds = run.cli().option_u64("rounds")?.unwrap_or(3).max(1);
    let route = run
        .route()
        .ok_or("the schema-friction benchmark is live and needs a route")?
        .clone();
    let options = route.turn_options(512);

    let mut records = Vec::new();
    for scenario in scenarios() {
        let record = run_scenario(&route, &scenario, options.clone(), rounds).await;
        run.record("schema", &record);
        records.push(record);
    }

    let report = render(&route, &records, rounds);
    run.table("schemas", &SUMMARY_HEADERS, summary_rows(&records));
    run.table("rounds", &ROUND_HEADERS, round_rows(&records));
    run.finish(report)?;
    Ok(())
}

async fn run_scenario(
    route: &norte_bench_support::Route,
    scenario: &Scenario,
    options: norte_core::TurnOptions,
    rounds: u64,
) -> SchemaRecord {
    let mut detail = Vec::new();
    let mut error = None;
    for round in 0..rounds {
        match one_round(route, scenario, options.clone(), round).await {
            Ok(round) => detail.push(round),
            Err(message) => error = Some(message),
        }
    }
    let mut micros: Vec<u64> = detail.iter().map(|round| round.micros).collect();
    micros.sort_unstable();
    SchemaRecord {
        schema: scenario.name.to_owned(),
        shape: scenario.shape.to_owned(),
        trap: scenario.trap.to_owned(),
        rounds,
        requests: detail.iter().map(|round| round.requests).sum(),
        calls: detail.iter().map(|round| round.calls).sum(),
        rejected: detail.iter().map(|round| round.rejected).sum(),
        accepted: detail.iter().map(|round| round.accepted).sum(),
        answered: detail.iter().filter(|round| round.answered).count() as u64,
        input_tokens: detail.iter().map(|round| round.input_tokens).sum(),
        output_tokens: detail.iter().map(|round| round.output_tokens).sum(),
        cost: detail.iter().map(|round| round.cost).sum(),
        median_micros: micros.get(micros.len() / 2).copied().unwrap_or(0),
        detail,
        error,
    }
}

async fn one_round(
    route: &norte_bench_support::Route,
    scenario: &Scenario,
    options: norte_core::TurnOptions,
    round: u64,
) -> Result<Round, String> {
    let captured = new_capture();
    let adapter = route
        .openai(text_capabilities())
        .map_err(|e| e.to_string())?;
    let probe = norte_bench_support::RecordingOpenAi::new(adapter, Arc::clone(&captured));
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(probe))
        .tools((scenario.build)().map_err(|e| e.to_string())?)
        .build()
        .map_err(|e| e.to_string())?;

    let session = engine.create().await.map_err(|e| e.to_string())?;
    let started = Instant::now();
    engine
        .turn(session.id)
        .input(Message::user(scenario.prompt))
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
    let (calls, rejected, accepted) = count_calls(&history);
    let answered = history
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .is_some_and(has_text);
    let mut totals = norte_bench_support::Counters::default();
    for message in &history {
        if message.role == Role::Assistant {
            if let Some(origin) = &message.origin {
                totals.add(&origin.usage);
            }
        }
    }
    let requests = captured
        .lock()
        .expect("the probe lock is never held across an await")
        .len();
    Ok(Round {
        schema: scenario.name.to_owned(),
        round,
        requests: u64::try_from(requests).unwrap_or(0),
        calls,
        rejected,
        accepted,
        answered,
        input_tokens: totals.input,
        output_tokens: totals.output,
        cost: totals.cost,
        micros,
    })
}

fn count_calls(history: &[Message]) -> (u64, u64, u64) {
    let mut calls = 0_u64;
    let mut rejected = 0_u64;
    let mut accepted = 0_u64;
    for message in history {
        for part in &message.parts {
            match part {
                Part::ToolCall(_) => calls += 1,
                Part::ToolResult(result) if result.is_error => rejected += 1,
                Part::ToolResult(_) => accepted += 1,
                _ => {}
            }
        }
    }
    (calls, rejected, accepted)
}

fn has_text(message: &Message) -> bool {
    message.parts.iter().any(|part| match part {
        Part::Text(text) => !text.text.trim().is_empty(),
        _ => false,
    })
}

fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

const SUMMARY_HEADERS: [&str; 11] = [
    "schema",
    "shape",
    "trap",
    "rounds",
    "requests",
    "calls",
    "rejected",
    "accepted",
    "answered",
    "median_micros",
    "error",
];

fn summary_rows(records: &[SchemaRecord]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|record| {
            vec![
                record.schema.clone(),
                record.shape.clone(),
                record.trap.clone(),
                record.rounds.to_string(),
                record.requests.to_string(),
                record.calls.to_string(),
                record.rejected.to_string(),
                record.accepted.to_string(),
                record.answered.to_string(),
                record.median_micros.to_string(),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

const ROUND_HEADERS: [&str; 10] = [
    "schema",
    "round",
    "requests",
    "calls",
    "rejected",
    "accepted",
    "answered",
    "input_tokens",
    "output_tokens",
    "cost",
];

fn round_rows(records: &[SchemaRecord]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for record in records {
        for round in &record.detail {
            rows.push(vec![
                round.schema.clone(),
                round.round.to_string(),
                round.requests.to_string(),
                round.calls.to_string(),
                round.rejected.to_string(),
                round.accepted.to_string(),
                round.answered.to_string(),
                round.input_tokens.to_string(),
                round.output_tokens.to_string(),
                format!("{:.6}", round.cost),
            ]);
        }
    }
    rows
}

#[allow(clippy::too_many_lines)]
fn render(route: &norte_bench_support::Route, records: &[SchemaRecord], rounds: u64) -> Report {
    let mut report = Report::new("Schema friction");
    report.paragraph(
        "When a tool call does not satisfy its schema, the engine commits the failure as a \
         tool error and asks the model again. So a hard schema is paid for in requests. \
         Each shape below is exercised with a trap ask, and the rejected calls are counted \
         rather than assumed.",
    );
    report.section("Run");
    report.kv(&[
        ("route", route.summary()),
        ("rounds per schema", rounds.to_string()),
        ("shapes", records.len().to_string()),
    ]);

    report.section("Shapes");
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            vec![
                record.schema.clone(),
                record.shape.clone(),
                record.trap.clone(),
                record.requests.to_string(),
                record.calls.to_string(),
                record.rejected.to_string(),
                record.accepted.to_string(),
                format!("{}/{}", record.answered, record.rounds),
                record.input_tokens.to_string(),
                record.output_tokens.to_string(),
                format!("{:.6}", record.cost),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    report.table(
        &[
            "schema", "shape", "trap", "requests", "calls", "rejected", "accepted", "answered",
            "input", "output", "cost", "error",
        ],
        &[
            Align::Left,
            Align::Left,
            Align::Left,
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
            .map(|record| (record.schema.clone(), record.rejected))
            .collect::<Vec<_>>(),
        24,
    );

    report.section("Per round");
    let rows: Vec<Vec<String>> = records
        .iter()
        .flat_map(|record| {
            record.detail.iter().map(|round| {
                vec![
                    round.schema.clone(),
                    round.round.to_string(),
                    round.requests.to_string(),
                    round.calls.to_string(),
                    round.rejected.to_string(),
                    round.accepted.to_string(),
                    round.answered.to_string(),
                    round.input_tokens.to_string(),
                    round.output_tokens.to_string(),
                    format!("{:.6}", round.cost),
                ]
            })
        })
        .collect();
    report.table(
        &[
            "schema", "round", "requests", "calls", "rejected", "accepted", "answered", "input",
            "output", "cost",
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
        ],
        &rows,
    );

    report.section("Reading");
    let rejected: u64 = records.iter().map(|record| record.rejected).sum();
    let requests: u64 = records.iter().map(|record| record.requests).sum();
    report.paragraph(&format!(
        "Across {requests} requests the schema rejected {rejected} call(s). A rejected call is \
         a whole extra request — the transcript, the schemas and the failed arguments again — \
         so the count is a cost, not a footnote. A shape with zero rejected calls is a schema \
         the model got right the first time."
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_declares_its_tools() {
        for scenario in scenarios() {
            let tools = (scenario.build)().expect("tools");
            assert_eq!(tools.len(), 1, "{}", scenario.name);
        }
    }
}
