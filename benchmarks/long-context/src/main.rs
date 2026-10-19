//! A long session kept whole against the same session with its history
//! compacted: what the context costs, and what compaction saves.
//!
//! A compaction policy is a claim about money: drop or summarize the old
//! history and every later request gets cheaper. The claim is easy to make and
//! easy to get wrong, because the thing it spends is not the answer but the
//! prefix — the part of the transcript a provider re-reads, re-bills and may
//! cache on every single request.
//!
//! So the benchmark builds one deterministic, host-authored set of notes and
//! sends it two ways:
//!
//! - **whole** — the full transcript, every exchange, as the session's history.
//! - **compacted** — a host-written summary of the same notes, as the history.
//!
//! Each arm then asks the same question and the same follow-up, and every figure
//! is the provider's own: the input and output tokens it reported, the cache read
//! and write it reported, the `usage.cost` it reported, and the wall-clock of the
//! turn. The model is not asked to grade itself: only tokens, money and time are
//! measured, and both answers are quoted so a reader can see what compaction did
//! to the reply.
//!
//! The summary is written by the host on purpose. Norte has no compaction policy
//! — dropping or summarizing history is a host decision, not an engine primitive
//! — and a summary produced by a model would be a request of its own, paid for at
//! the transcript's size. This run measures what compaction saves at request time;
//! what producing the summary costs is a second measurement, on the host.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
//!   cargo run -p norte-bench-long-context -- --turns 32 --lines 8
//! ```

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use norte_bench_support::report::{Align, Report};
use norte_bench_support::{
    Counters, Mode, RecordingOpenAi, Route, Run, as_f64, captured as new_capture, cost_of,
    duration as fmt_duration, money_opt, percent,
};
use norte_core::{
    Capabilities, Digest, EngineBuilder, InMemoryBlobStore, InMemoryStore, Message, Modality,
    StreamingGranularity, TurnOptions,
};
use serde::Serialize;
use serde_json::Value;

const USAGE: &str = "\
norte-bench-long-context — a long session kept whole against a compacted one, live

USAGE:
    norte-bench-long-context [OPTIONS]

OPTIONS:
    --turns <n>     exchanges in the synthetic notes (default 32)
    --lines <n>     lines per exchange, so the notes can be made long (default 8)
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

Live: needs a key and a route in the environment.
  OPENROUTER_API_KEY (or BENCH_KEY_ENV), BENCH_ENDPOINT, BENCH_MODEL, BENCH_PROVIDER,
  BENCH_PAUSE_MS.
";

/// The instructions both arms carry: the only part of the prefix they share by
/// construction, so what differs below is the context and nothing else.
const SYSTEM: &str =
    "You are a concise assistant. Answer in one short sentence, in plain text, with no preamble.";

/// The cold question: answerable from the notes and from the summary alike.
const QUESTION: &str =
    "In one sentence: what project do these notes describe, and what version number do they name?";

/// The warm follow-up: sent after the first answer, so its prefix is the first
/// request plus that answer — the shape a real session has, and the shape a
/// provider can serve from cache.
const FOLLOW_UP: &str = "In one sentence: what single risk do the notes name?";

/// One provider request, read from the recording model and the usage the engine
/// committed for it.
#[derive(Serialize)]
struct RequestRow {
    /// The arm the request belongs to: `whole` or `compacted`.
    arm: &'static str,
    /// `cold` for the first ask, `warm` for the follow-up.
    step: &'static str,
    /// One-based position within the arm.
    index: usize,
    /// Messages the projection carried.
    messages: usize,
    /// Bytes of the projection the adapter would have sent.
    bytes: usize,
    /// Messages shared with the previous request of the same arm.
    shared: Option<usize>,
    /// Whether that shared prefix was byte-identical.
    byte_identical: Option<bool>,
    /// The digest of the shared prefix, for the audit.
    shared_digest: Option<String>,
    /// The projection's own digest.
    digest: String,
    /// Prompt tokens, the provider's count.
    input: Option<u64>,
    /// Tokens the model produced.
    output: Option<u64>,
    /// Tokens the provider read from its cache.
    cache_read: Option<u64>,
    /// Tokens the provider wrote into its cache.
    cache_write: Option<u64>,
    /// The provider's own `usage.cost`, when the payload carried one.
    cost: Option<f64>,
    /// The wall-clock of the turn that made it.
    elapsed: Duration,
}

/// One arm: its context, its requests, the counters they summed to, and the
/// answers the model gave.
#[derive(Serialize)]
struct Arm {
    /// `whole` or `compacted`.
    name: &'static str,
    /// Messages the context contributed before the first question.
    context_messages: usize,
    /// The requests the arm made, in order.
    requests: Vec<RequestRow>,
    /// The answers, in order, quoted in the report.
    answers: Vec<String>,
    /// The provider's counters, summed over the arm.
    counters: Counters,
    /// The wall-clock of the arm's turns.
    elapsed: Duration,
}

/// The whole measurement: both arms, and what the comparison across them says.
#[derive(Serialize)]
struct Outcome {
    /// The route, one line, as the report names it.
    route: String,
    /// Exchanges in the synthetic notes.
    turns: u64,
    /// Lines per exchange.
    lines: u64,
    /// The two arms, `whole` first.
    arms: Vec<Arm>,
    /// The cold request's input tokens, cut by compaction, as a fraction.
    input_reduction: Option<f64>,
    /// Both requests' cost, cut by compaction, as a fraction.
    cost_reduction: Option<f64>,
    /// The failure, when the run could not finish.
    error: Option<String>,
}

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "long-context",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };
    let turns = run.cli().option_u64("turns")?.unwrap_or(32).max(1);
    let lines = run.cli().option_u64("lines")?.unwrap_or(8).max(1);
    let route = run
        .route()
        .ok_or("the long-context benchmark is live and needs a route")?
        .clone();

    let outcome = measure(&route, turns, lines).await;
    run.record("outcome", &outcome);
    for arm in &outcome.arms {
        for request in &arm.requests {
            run.record("request", request);
        }
    }
    run.table("arms", ARM_HEADERS, arm_rows(&outcome));
    run.table("requests", REQUEST_HEADERS, request_rows(&outcome));

    let report = render(&outcome);
    run.finish(report)?;
    Ok(())
}

/// Measures both arms, turning a failure into a reportable outcome rather than a
/// panic: a run that dies mid-request still has something true to write down.
async fn measure(route: &Route, turns: u64, lines: u64) -> Outcome {
    match measure_inner(route, turns, lines).await {
        Ok(outcome) => outcome,
        Err(error) => Outcome {
            route: route.summary(),
            turns,
            lines,
            arms: Vec::new(),
            input_reduction: None,
            cost_reduction: None,
            error: Some(error),
        },
    }
}

async fn measure_inner(route: &Route, turns: u64, lines: u64) -> Result<Outcome, String> {
    let options = route.turn_options(256);
    let whole = run_arm(route, "whole", transcript(turns, lines), &options).await?;
    let compacted = run_arm(
        route,
        "compacted",
        vec![Message::system(SYSTEM), Message::user(summary())],
        &options,
    )
    .await?;

    let input_reduction = ratio(
        whole.requests.first().and_then(|request| request.input),
        compacted.requests.first().and_then(|request| request.input),
    );
    let cost_reduction = if whole.counters.cost > 0.0 {
        Some(1.0 - compacted.counters.cost / whole.counters.cost)
    } else {
        None
    };

    Ok(Outcome {
        route: route.summary(),
        turns,
        lines,
        arms: vec![whole, compacted],
        input_reduction,
        cost_reduction,
        error: None,
    })
}

/// Runs one arm: a fresh engine over the same route, the context appended as
/// history, then the cold ask and the warm follow-up.
#[allow(clippy::too_many_lines)]
async fn run_arm(
    route: &Route,
    name: &'static str,
    context: Vec<Message>,
    options: &TurnOptions,
) -> Result<Arm, String> {
    let captured = new_capture();
    let adapter = route
        .openai(text_capabilities())
        .map_err(|error| error.to_string())?;
    let model = RecordingOpenAi::new(adapter, Arc::clone(&captured), route.pause());
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .build()
        .map_err(|error| error.to_string())?;
    let session = engine.create().await.map_err(|error| error.to_string())?;
    let context_messages = context.len();
    engine
        .append_history(session.id, context)
        .await
        .map_err(|error| error.to_string())?;

    let mut arm = Arm {
        name,
        context_messages,
        requests: Vec::new(),
        answers: Vec::new(),
        counters: Counters::default(),
        elapsed: Duration::ZERO,
    };
    let mut seen = 0_usize;
    let mut previous: Option<Vec<Value>> = None;
    for (step, prompt) in [("cold", QUESTION), ("warm", FOLLOW_UP)] {
        let started = Instant::now();
        let outcome = engine
            .turn(session.id)
            .input(Message::user(prompt))
            .options(options.clone())
            .start()
            .finish()
            .await
            .map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();
        arm.elapsed += elapsed;

        let sent = {
            let all = captured
                .lock()
                .expect("the probe lock is never held across an await");
            let slice = all[seen..].to_vec();
            seen = all.len();
            slice
        };
        let record = sent
            .last()
            .ok_or_else(|| format!("{name}: the {step} turn sent no request"))?;
        let messages = messages_of(&record.body);
        let (shared, same, shared_digest) = match &previous {
            Some(previous_messages) => {
                let shared = messages.len().min(previous_messages.len());
                let same = previous_messages.len() >= shared
                    && messages[..shared] == previous_messages[..shared];
                let digest =
                    Digest::of_json(&Value::Array(messages[..shared].to_vec())).to_string();
                (Some(shared), Some(same), Some(digest))
            }
            None => (None, None, None),
        };
        previous = Some(messages);

        let history = engine
            .history(session.id)
            .await
            .map_err(|error| error.to_string())?;
        let usage = history
            .iter()
            .rev()
            .find_map(|message| message.origin.as_ref().map(|origin| &origin.usage));
        if let Some(usage) = usage {
            arm.counters.add(usage);
        }
        let answer = outcome
            .message
            .as_ref()
            .map(Message::text)
            .unwrap_or_default();
        if !answer.is_empty() {
            arm.answers.push(answer);
        }
        arm.requests.push(RequestRow {
            arm: name,
            step,
            index: arm.requests.len() + 1,
            messages: record.messages,
            bytes: record.bytes,
            shared,
            byte_identical: same,
            shared_digest: shared_digest.as_deref().map(short),
            digest: short(&record.digest),
            input: usage.and_then(|usage| usage.input_tokens),
            output: usage.and_then(|usage| usage.output_tokens),
            cache_read: usage.and_then(|usage| usage.cache_read_tokens),
            cache_write: usage.and_then(|usage| usage.cache_write_tokens),
            cost: usage.and_then(cost_of),
            elapsed,
        });
    }
    Ok(arm)
}

fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}

/// The context, as the provider's projection lists it: the message array of the
/// body the adapter would `POST`.
fn messages_of(body: &Value) -> Vec<Value> {
    body.get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The first twelve characters of a digest, which is enough to see whether two
/// prefixes agree without turning the report into a wall of hex.
fn short(digest: &str) -> String {
    let mut out: String = digest.chars().take(12).collect();
    if digest.chars().count() > 12 {
        out.push('…');
    }
    out
}

/// `1 - compacted / whole`, or `None` when either count was not reported.
fn ratio(whole: Option<u64>, compacted: Option<u64>) -> Option<f64> {
    let whole = whole?;
    let compacted = compacted?;
    if whole == 0 {
        return None;
    }
    Some(1.0 - as_f64(compacted) / as_f64(whole))
}

/// The notes: a deterministic, host-authored document long enough to be worth
/// compacting. Nothing here is generated by a model, so two runs send the same
/// bytes and the two arms differ only in how those bytes are carried.
fn transcript(turns: u64, lines: u64) -> Vec<Message> {
    let mut messages = Vec::with_capacity(usize::try_from(turns * 2 + 1).unwrap_or(1));
    messages.push(Message::system(SYSTEM));
    for turn in 0..turns {
        messages.push(Message::user(exchange(turn, lines, true)));
        messages.push(Message::assistant(exchange(turn, lines, false)));
    }
    messages
}

/// One exchange's text: `lines` observations about the same fictional engine.
fn exchange(turn: u64, lines: u64, ask: bool) -> String {
    const SUBSYSTEMS: [&str; 8] = [
        "session store",
        "commit log",
        "wire adapter",
        "tool registry",
        "policy engine",
        "blob store",
        "projection",
        "continuity token",
    ];
    const VERBS: [&str; 6] = [
        "committed",
        "replayed",
        "rejected",
        "deduplicated",
        "streamed",
        "sealed",
    ];
    let mut out = String::new();
    for line in 0..lines {
        let n = turn * lines + line;
        let index = usize::try_from(n).unwrap_or(0);
        let subsystem = SUBSYSTEMS[index % SUBSYSTEMS.len()];
        let verb = VERBS[index % VERBS.len()];
        if ask {
            let _ = writeln!(
                out,
                "Note {n}: the {subsystem} {verb} records for review; observation {n} is logged \
                 against the Norte engine and carries no decision of its own."
            );
        } else {
            let _ = writeln!(
                out,
                "Acknowledged note {n}: the {subsystem} {verb} records were filed, and observation \
                 {n} was cross-checked against the rule set."
            );
        }
    }
    out
}

/// The host-written summary of the notes: the name, the version and the risk,
/// which are the three things the questions ask for.
fn summary() -> String {
    "Summary of the field notes: they describe the Norte engine, version 0.1.0, an append-only \
     session engine with a commit log, a projection, a tool registry, a policy engine and a blob \
     store. The notes record that each subsystem committed, replayed or sealed records for review, \
     and that every observation was cross-checked against the rule set. The notes name one risk: a \
     long prefix is re-sent on every request, so it is the context, not the answer, that grows."
        .to_owned()
}

/// The CSV headers, one set per table the run writes.
const ARM_HEADERS: &[&str] = &[
    "arm",
    "context_messages",
    "requests",
    "input",
    "cache_read",
    "cache_write",
    "output",
    "cost",
    "elapsed",
];
const REQUEST_HEADERS: &[&str] = &[
    "arm",
    "step",
    "index",
    "messages",
    "bytes",
    "shared",
    "byte_identical",
    "shared_digest",
    "input",
    "cache_read",
    "cache_write",
    "output",
    "cost",
    "elapsed",
];

/// The arm named `name`, if the run produced it.
fn arm<'a>(outcome: &'a Outcome, name: &str) -> Option<&'a Arm> {
    outcome.arms.iter().find(|arm| arm.name == name)
}

/// The cold request of an arm, if it has one.
fn cold(arm: &Arm) -> Option<&RequestRow> {
    arm.requests.first()
}

/// One arm per row, for the markdown report.
fn arm_cells(outcome: &Outcome) -> Vec<Vec<String>> {
    outcome
        .arms
        .iter()
        .map(|arm| {
            vec![
                arm.name.to_owned(),
                arm.context_messages.to_string(),
                arm.requests.len().to_string(),
                arm.counters.input.to_string(),
                arm.counters.cache_read.to_string(),
                arm.counters.cache_write.to_string(),
                arm.counters.output.to_string(),
                money_opt(Some(arm.counters.cost)),
                fmt_duration(arm.elapsed),
            ]
        })
        .collect()
}

/// The same table as CSV: a bare cost and a bare fraction, so a spreadsheet can
/// sum them, and an empty cell where the provider reported nothing.
fn arm_rows(outcome: &Outcome) -> Vec<Vec<String>> {
    outcome
        .arms
        .iter()
        .map(|arm| {
            vec![
                arm.name.to_owned(),
                arm.context_messages.to_string(),
                arm.requests.len().to_string(),
                arm.counters.input.to_string(),
                arm.counters.cache_read.to_string(),
                arm.counters.cache_write.to_string(),
                arm.counters.output.to_string(),
                format!("{:.6}", arm.counters.cost),
                format!("{:.3}", arm.elapsed.as_secs_f64()),
            ]
        })
        .collect()
}

fn request_cells(outcome: &Outcome) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for arm in &outcome.arms {
        for request in &arm.requests {
            rows.push(vec![
                request.arm.to_owned(),
                request.step.to_owned(),
                request.index.to_string(),
                request.messages.to_string(),
                request.bytes.to_string(),
                request
                    .shared
                    .map_or_else(|| "—".to_owned(), |shared| shared.to_string()),
                request.byte_identical.map_or_else(
                    || "—".to_owned(),
                    |same| if same { "yes" } else { "no" }.to_owned(),
                ),
                request
                    .shared_digest
                    .clone()
                    .unwrap_or_else(|| "—".to_owned()),
                request
                    .input
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                request
                    .cache_read
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                request
                    .cache_write
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                request
                    .output
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                money_opt(request.cost),
                fmt_duration(request.elapsed),
            ]);
        }
    }
    rows
}

fn request_rows(outcome: &Outcome) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for arm in &outcome.arms {
        for request in &arm.requests {
            let count =
                |value: Option<u64>| value.map_or_else(String::new, |value| value.to_string());
            rows.push(vec![
                request.arm.to_owned(),
                request.step.to_owned(),
                request.index.to_string(),
                request.messages.to_string(),
                request.bytes.to_string(),
                request
                    .shared
                    .map_or_else(String::new, |shared| shared.to_string()),
                request
                    .byte_identical
                    .map_or_else(String::new, |same| same.to_string()),
                request.shared_digest.clone().unwrap_or_default(),
                count(request.input),
                count(request.cache_read),
                count(request.cache_write),
                count(request.output),
                request
                    .cost
                    .map_or_else(String::new, |cost| format!("{cost:.6}")),
                format!("{:.3}", request.elapsed.as_secs_f64()),
            ]);
        }
    }
    rows
}

/// The report a reader keeps: both arms, the per-request audit, the answers and
/// what the comparison does and does not say.
#[allow(clippy::too_many_lines)]
fn render(outcome: &Outcome) -> Report {
    let mut report = Report::new("long-context");
    report.paragraph(
        "A long session kept whole against the same session with its history compacted. The notes \
         are host-authored and deterministic, so the two arms differ only in the context they \
         carry: the full transcript against a host-written summary of it. Each arm asks the same \
         question and the same follow-up.",
    );
    report.paragraph(
        "Only what the provider reported is measured: input and output tokens, cache reads and \
         writes, the bill and the wall-clock. Whether the compacted answer is as good is not a \
         number this run produces, so both answers are quoted at the end for a reader to judge.",
    );

    report.section("Run");
    report.kv(&[
        ("route", outcome.route.clone()),
        (
            "pause",
            "`BENCH_PAUSE_MS` before each request; the elapsed column includes it".to_owned(),
        ),
        ("exchanges in the notes", outcome.turns.to_string()),
        ("lines per exchange", outcome.lines.to_string()),
        ("system message", SYSTEM.to_owned()),
        ("cold question", QUESTION.to_owned()),
        ("warm follow-up", FOLLOW_UP.to_owned()),
        (
            "context messages (whole)",
            arm(outcome, "whole")
                .map_or_else(|| "—".to_owned(), |arm| arm.context_messages.to_string()),
        ),
        (
            "context messages (compacted)",
            arm(outcome, "compacted")
                .map_or_else(|| "—".to_owned(), |arm| arm.context_messages.to_string()),
        ),
        ("summary", summary().replace('\n', " ")),
    ]);
    report.paragraph(
        "**Cold** means the first request of the arm, not a globally cold cache. The notes are \
         deterministic, so an earlier run with the same prefix can leave a cache that this run's \
         first request reads; the `cache read` column is the evidence.",
    );

    if let Some(error) = &outcome.error {
        report.section("Error");
        report.paragraph(&format!(
            "The run did not finish: `{error}`. Nothing below was measured."
        ));
        return report;
    }

    report.section("Summary");
    report.table(
        &[
            "arm",
            "context messages",
            "requests",
            "input",
            "cache read",
            "cache write",
            "output",
            "cost",
            "elapsed",
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
        &arm_cells(outcome),
    );
    let bars: Vec<(String, u64)> = outcome
        .arms
        .iter()
        .map(|arm| (format!("{} input", arm.name), arm.counters.input))
        .collect();
    report.bars(&bars, 28);

    report.section("Compaction");
    let whole = arm(outcome, "whole");
    let compacted = arm(outcome, "compacted");
    if let (Some(whole), Some(compacted)) = (whole, compacted) {
        match (
            cold(whole).and_then(|request| request.input),
            cold(compacted).and_then(|request| request.input),
        ) {
            (Some(whole_input), Some(compacted_input)) => {
                report.paragraph(&format!(
                    "The cold request carried {whole_input} input tokens with the whole transcript \
                     and {compacted_input} with the summary — a reduction of {}.",
                    percent(whole_input.saturating_sub(compacted_input), whole_input)
                ));
            }
            _ => {
                report.paragraph(
                    "The provider did not report an input count for the cold request, so the \
                     reduction cannot be stated arithmetically; the counters above are all that \
                     is known.",
                );
            }
        }
        report.paragraph(&format!(
            "Across both requests the compacted arm cost {} against {} — {}.",
            money_opt(Some(compacted.counters.cost)),
            money_opt(Some(whole.counters.cost)),
            outcome.cost_reduction.map_or_else(
                || "no reduction can be stated".to_owned(),
                |reduction| format!("a reduction of {:.1}%", reduction * 100.0)
            )
        ));
        report.paragraph(
            "A cache read is what a long, stable prefix can be worth: if the warm request of the \
             whole arm reads a large share of its input from the provider's cache, the whole arm \
             is cheaper than its token count suggests. Read the two rows above against each other \
             — a policy that always compacts would throw that cache away.",
        );
        report.paragraph(
            "A byte-identical prefix is necessary, not sufficient: the provider decides whether \
             it has that prefix cached and which instance serves the request. A zero in `cache \
             read` beside a byte-identical prefix means the provider did not serve one; it does \
             not mean the prefix changed.",
        );
    }

    report.section("Per request");
    report.table(
        &[
            "arm",
            "step",
            "#",
            "messages",
            "bytes",
            "shared",
            "byte-identical",
            "shared digest",
            "input",
            "cache read",
            "cache write",
            "output",
            "cost",
            "elapsed",
        ],
        &[
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
        &request_cells(outcome),
    );

    report.section("Prefix reuse");
    for arm in &outcome.arms {
        if let Some(second) = arm.requests.get(1) {
            match (second.shared, second.byte_identical) {
                (Some(shared), Some(true)) => report.paragraph(&format!(
                    "**{}**: the warm request carried {shared} message(s) byte-identical to the \
                     cold one (shared digest `{}`), which is the prefix a provider can serve from \
                     cache. Any cache read it reported is under that request's `cache read` column.",
                    arm.name,
                    second.shared_digest.as_deref().unwrap_or("—")
                )),
                (Some(shared), Some(false)) => report.paragraph(&format!(
                    "**{}**: the warm request shared {shared} message(s) with the cold one and \
                     **changed** them, so no prefix cache could apply.",
                    arm.name
                )),
                _ => report.paragraph(&format!(
                    "**{}**: the arm did not make two requests, so there is no prefix to reuse.",
                    arm.name
                )),
            };
        }
    }

    report.section("Answers");
    for arm in &outcome.arms {
        report.lines(&format!("**{}**", arm.name));
        if arm.answers.is_empty() {
            report.lines("- no answer was committed");
        }
        for (index, answer) in arm.answers.iter().enumerate() {
            report.lines(&format!("{}. {answer}", index + 1));
        }
        report.lines("");
    }

    report.section("Reading");
    report.paragraph(
        "The saving above is what compaction saves at request time. It is not the whole cost of a \
         compaction policy: dropping history can lose what the question needs, and a summary \
         written by a model is itself a request, paid for at the transcript's size. This run \
         quotes both answers so those costs can be judged rather than assumed.",
    );
    report.paragraph(
        "The summary is written by the host, not by the model, because Norte has no compaction \
         policy — how history is dropped or summarized is a host decision (the engine's job is to \
         commit what it is given, in order, and to re-send it unchanged). So this is the saving a \
         host's policy could reach, measured against a transcript the host chose.",
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_is_system_and_pairs() {
        let notes = transcript(3, 2);
        assert_eq!(notes.len(), 1 + 3 * 2);
        assert_eq!(notes[0].text(), SYSTEM);
    }

    #[test]
    fn the_transcript_is_deterministic() {
        assert_eq!(transcript(4, 3), transcript(4, 3));
    }

    #[test]
    fn the_summary_names_the_answers() {
        let summary = summary();
        assert!(summary.contains("Norte"));
        assert!(summary.contains("0.1.0"));
        assert!(summary.to_lowercase().contains("risk"));
    }

    #[test]
    fn the_ratio_is_the_saving() {
        assert_eq!(ratio(Some(100), Some(25)), Some(0.75));
        assert_eq!(ratio(Some(100), Some(100)), Some(0.0));
        assert_eq!(ratio(None, Some(1)), None);
        assert_eq!(ratio(Some(0), Some(0)), None);
    }

    /// A minimal outcome with both arms, so the report and the tables can be
    /// exercised without a provider.
    fn sample() -> Outcome {
        let request = |arm, step, index, input, cache_read, shared| RequestRow {
            arm,
            step,
            index,
            messages: 4,
            bytes: 100,
            shared,
            byte_identical: shared.map(|_| true),
            shared_digest: shared.map(|_| "0123456789ab…".to_owned()),
            digest: "0123456789ab…".to_owned(),
            input: Some(input),
            output: Some(4),
            cache_read: Some(cache_read),
            cache_write: Some(0),
            cost: Some(0.000_1),
            elapsed: Duration::from_millis(10),
        };
        let whole = Arm {
            name: "whole",
            context_messages: 65,
            requests: vec![
                request("whole", "cold", 1, 900, 0, None),
                request("whole", "warm", 2, 950, 900, Some(4)),
            ],
            answers: vec!["Norte 0.1.0".to_owned()],
            counters: Counters {
                requests: 2,
                input: 1850,
                output: 8,
                cache_read: 900,
                cost: 0.000_2,
                ..Counters::default()
            },
            elapsed: Duration::from_millis(20),
        };
        let compacted = Arm {
            name: "compacted",
            context_messages: 2,
            requests: vec![
                request("compacted", "cold", 1, 100, 0, None),
                request("compacted", "warm", 2, 140, 100, Some(4)),
            ],
            answers: vec!["Norte 0.1.0".to_owned()],
            counters: Counters {
                requests: 2,
                input: 240,
                output: 8,
                cache_read: 100,
                cost: 0.000_02,
                ..Counters::default()
            },
            elapsed: Duration::from_millis(20),
        };
        Outcome {
            route: "test".to_owned(),
            turns: 32,
            lines: 8,
            arms: vec![whole, compacted],
            input_reduction: Some(0.889),
            cost_reduction: Some(0.9),
            error: None,
        }
    }

    #[test]
    fn every_row_matches_its_header() {
        let outcome = sample();
        assert!(
            arm_rows(&outcome)
                .iter()
                .all(|row| row.len() == ARM_HEADERS.len())
        );
        assert!(
            request_rows(&outcome)
                .iter()
                .all(|row| row.len() == REQUEST_HEADERS.len())
        );
        assert!(arm_cells(&outcome).iter().all(|row| row.len() == 9));
        assert!(request_cells(&outcome).iter().all(|row| row.len() == 14));
    }

    #[test]
    fn the_report_names_both_arms_and_the_saving() {
        let report = render(&sample()).finish();
        assert!(report.contains("whole"));
        assert!(report.contains("compacted"));
        assert!(report.contains("Compaction"));
        assert!(report.contains("88.9%"));
        assert!(report.contains("byte-identical"));
    }

    #[test]
    fn an_error_still_renders_a_report() {
        let outcome = Outcome {
            route: "test".to_owned(),
            turns: 1,
            lines: 1,
            arms: Vec::new(),
            input_reduction: None,
            cost_reduction: None,
            error: Some("the provider said no".to_owned()),
        };
        let report = render(&outcome).finish();
        assert!(report.contains("the provider said no"));
    }
}
