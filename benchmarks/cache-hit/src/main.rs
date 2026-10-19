//! The provider cache, measured: what survives a turn, a tool call, a park and
//! an export — and what each request cost and took.
//!
//! Two different things are being watched, and they belong to different owners.
//! The **bytes** are the library's: reconstruction is append-only and
//! deterministic (FR-RECON-1, FR-RECON-2), so every request starts with exactly
//! the message prefix of the one before it, and a request that grows only at the
//! end is a request whose prefix a provider may still hold. The **tokens** are
//! the provider's: it reports what it actually read from its cache in
//! `Usage::cache_read_tokens`, and that is the only number that says a cache hit
//! really happened.
//!
//! So every phase prints both: the usage the provider reported, and a digest of
//! the message prefix shared with the previous request — the attribution. A
//! cache read that is not explained by an identical prefix would be a bug in the
//! library; an identical prefix with no cache read is the provider deciding
//! (a short prefix, an expired entry, a model that does not cache at all).
//!
//! The four phases are the four things a host worries about:
//!
//! 1. the normal case — the next turn re-sends the conversation;
//! 2. after a tool call — the tool exchange joined the prefix;
//! 3. after human approval — the turn parked, and the resume re-sends what the
//!    model already answered;
//! 4. after an export and an import into a fresh engine — the transcript is new
//!    memory, the bytes must not be.
//!
//! Every request is reported from the committed history, so the intermediate
//! request inside a tool loop is counted too. The turn's wall-clock is measured
//! around `finish()`, and the bill is the `usage.cost` the provider itself
//! reported; no price table lives here.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//! BENCH_PROVIDER=DeepInfra \
//! cargo run -p norte-bench-cache-hit
//! ```
//!
//! The prefix has to be worth caching before any of this is visible: providers
//! only cache a prefix past a floor (often about a thousand tokens), so the
//! system message below is a long one.
//!
//! And whether a hit is *reported* is the provider's: the model here is whatever
//! `BENCH_MODEL` names, and a free model's provider reports cache usage only
//! when it keeps a cache and says so. `BENCH_MODEL=openai/gpt-6-luna` runs the
//! identical recipe against a provider that reports it, so the two halves — the
//! bytes, which the library controls, and the tokens, which it does not — can be
//! watched next to each other.
//!
//! `BENCH_PROVIDER=OpenInference` pins the upstream provider. Without it
//! `OpenRouter` may route two turns of the same conversation to different
//! providers, and each provider keeps its own cache: the hit is then a routing
//! decision, not a property of the prefix.
//!
//! A run leaves a report behind under `results/`, named after the moment and the
//! route; `--report <path>` writes it elsewhere.

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use norte_bench_support::report::{Align, Report};
use norte_bench_support::{
    Captured, Mode, RecordingOpenAi, RequestRecord, Route, Run, captured as new_capture, duration,
    money_opt, tokens,
};
use norte_core::{
    Capabilities, Digest, Engine, EngineBuilder, FnExecutor, GrantScope, InMemoryBlobStore,
    InMemoryStore, Message, Modality, PolicySet, Role, SessionId, StreamingGranularity,
    ToolApprovalPolicy, ToolKind, ToolName, ToolRegistry, TurnOptions, Usage,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const USAGE: &str = "\
cache-hit — what survives a turn, a tool call, a park and an export, live

USAGE:
    norte-bench-cache-hit [OPTIONS]

OPTIONS:
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

Live: needs a key and a route in the environment.
  OPENROUTER_API_KEY (or BENCH_KEY_ENV), BENCH_ENDPOINT, BENCH_MODEL, BENCH_PROVIDER,
  BENCH_PAUSE_MS.
";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "cache-hit",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };
    let route = run
        .route()
        .ok_or("the cache-hit benchmark is live and needs a route")?
        .clone();
    let started = Utc::now();

    let (engine, captured) = new_engine(&route)?;
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(manual())])
        .await?;
    let mut seen = engine.history(session.id).await?.len();
    let mut bench = Bench::default();
    let options = route.turn_options(512);

    println!(
        "--- route: {}",
        route.provider.as_deref().map_or_else(
            || "OpenRouter decides, and may switch upstream between turns".to_owned(),
            |provider| format!("pinned to {provider}")
        )
    );

    println!("--- 1. the normal case");
    normal(
        &engine, session.id, &captured, &mut bench, &mut seen, &options,
    )
    .await?;

    println!("\n--- 2. after a tool call");
    after_tool(
        &engine, session.id, &captured, &mut bench, &mut seen, &options,
    )
    .await?;

    println!("\n--- 3. after human approval");
    after_approval(
        &engine, session.id, &captured, &mut bench, &mut seen, &options,
    )
    .await?;

    println!("\n--- 4. after an export and an import into a fresh engine");
    after_export(&engine, &captured, session.id, &mut bench, &options, &route).await?;

    let closing = bench.closing();
    println!("\n--- what this shows\n{closing}");

    let report = render_report(&bench, &closing, &route, started);
    for turn in &bench.turns {
        run.record("turn", turn);
    }
    for request in &bench.requests {
        run.record("request", request);
    }
    for audit in &bench.audits {
        run.record("prefix", audit);
    }
    run.table("turns", TURN_HEADERS, turn_cells(&bench));
    run.table("requests", REQUEST_HEADERS, request_rows(&bench));
    run.table("prefix-audit", AUDIT_HEADERS, audit_rows(&bench));
    run.finish(report)?;
    Ok(())
}

/// The normal case: the next turn re-sends the conversation, and that is all.
async fn normal(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    bench: &mut Bench,
    seen: &mut usize,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let _ = engine
        .turn(session)
        .input(Message::user(ASK_A))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn(
        "1. normal",
        "turn 1 (nothing to reuse yet)",
        started.elapsed(),
    );
    bench
        .collect(
            engine,
            session,
            seen,
            "1. normal",
            "turn 1 (nothing to reuse yet)",
        )
        .await?;

    let started = Instant::now();
    let _ = engine
        .turn(session)
        .input(Message::user(ASK_B))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn(
        "1. normal",
        "turn 2 (the conversation again)",
        started.elapsed(),
    );
    bench
        .collect(
            engine,
            session,
            seen,
            "1. normal",
            "turn 2 (the conversation again)",
        )
        .await?;

    bench.audit("1. normal", captured, None);
    Ok(())
}

/// After a tool call: the call and its result join the prefix, at the end.
async fn after_tool(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    bench: &mut Bench,
    seen: &mut usize,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let _ = engine
        .turn(session)
        .input(Message::user(COUNT_WORDS))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn("2. tool", "the turn that calls the tool", started.elapsed());
    bench
        .collect(
            engine,
            session,
            seen,
            "2. tool",
            "the turn that calls the tool",
        )
        .await?;

    let started = Instant::now();
    let _ = engine
        .turn(session)
        .input(Message::user("How many was that?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn("2. tool", "the turn after the result", started.elapsed());
    bench
        .collect(
            engine,
            session,
            seen,
            "2. tool",
            "the turn after the result",
        )
        .await?;

    bench.audit("2. tool", captured, None);
    Ok(())
}

/// After approval: the parked turn is not rebuilt when it resumes — the resume
/// re-sends the same prefix the park already sent.
async fn after_approval(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    bench: &mut Bench,
    seen: &mut usize,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let outcome = engine
        .turn(session)
        .input(Message::user(DEPLOY))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn("3. approval", "the turn that parks", started.elapsed());
    bench
        .collect(engine, session, seen, "3. approval", "the turn that parks")
        .await?;
    println!("phase: {:?}", outcome.session.phase);
    for record in engine.pending_approvals(session).await? {
        println!("waiting on {} (approval {})", record.tool, record.id);
        engine
            .approve(record.id, GrantScope::Call(record.call_id), "operator")
            .await?;
    }

    let started = Instant::now();
    let _ = engine.resume(session).finish().await?;
    bench.turn(
        "3. approval",
        "the resume (not a new request: the parked one)",
        started.elapsed(),
    );
    bench
        .collect(
            engine,
            session,
            seen,
            "3. approval",
            "the resume (not a new request: the parked one)",
        )
        .await?;

    let started = Instant::now();
    let _ = engine
        .turn(session)
        .input(Message::user("What did the deploy return?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn(
        "3. approval",
        "the turn after the approval",
        started.elapsed(),
    );
    bench
        .collect(
            engine,
            session,
            seen,
            "3. approval",
            "the turn after the approval",
        )
        .await?;

    bench.audit("3. approval", captured, None);
    Ok(())
}

/// After an export: the conversation is imported into a fresh engine and asked
/// to carry on — the transcript is new memory, the bytes must not be.
async fn after_export(
    engine: &Engine,
    captured: &Captured,
    session: SessionId,
    bench: &mut Bench,
    options: &TurnOptions,
    route: &Route,
) -> Result<(), Box<dyn Error>> {
    let conversation = engine.export(session).await?;
    println!(
        "envelope: session {} · {} message(s) · {} bytes of JSON",
        conversation.session,
        conversation.messages.len(),
        conversation.to_json()?.len()
    );
    let (elsewhere, requests) = new_engine(route)?;
    let restored = elsewhere.import(conversation).await?;
    println!(
        "restored session: {} ({})",
        restored.id,
        if restored.id == session {
            "the same id"
        } else {
            "A DIFFERENT ID"
        }
    );

    let mut restored_seen = elsewhere.history(restored.id).await?.len();
    let started = Instant::now();
    let _ = elsewhere
        .turn(restored.id)
        .input(Message::user("In one line: what have we done so far?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    bench.turn(
        "4. export",
        "the first turn of the new process",
        started.elapsed(),
    );
    bench
        .collect(
            &elsewhere,
            restored.id,
            &mut restored_seen,
            "4. export",
            "the first turn of the new process",
        )
        .await?;

    bench.audit("4. export", &requests, Some(captured));
    Ok(())
}

// ── the measurement ─────────────────────────────────────────────────────────

/// One turn's wall-clock, and the phase it belongs to.
#[derive(Serialize)]
struct TurnRow {
    phase: &'static str,
    label: &'static str,
    elapsed: Duration,
}

/// One provider request, read from the assistant message it committed.
#[derive(Serialize)]
struct RequestRow {
    phase: &'static str,
    turn: &'static str,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    cost: Option<f64>,
}

/// One request's place in the prefix audit.
#[derive(Serialize)]
struct AuditRow {
    phase: &'static str,
    label: String,
    messages: usize,
    bytes: usize,
    shared: Option<usize>,
    same: Option<bool>,
    digest: Option<String>,
}

#[derive(Default)]
struct Bench {
    turns: Vec<TurnRow>,
    requests: Vec<RequestRow>,
    audits: Vec<AuditRow>,
}

impl Bench {
    fn turn(&mut self, phase: &'static str, label: &'static str, elapsed: Duration) {
        self.turns.push(TurnRow {
            phase,
            label,
            elapsed,
        });
    }

    /// Reads the assistant messages committed since the last call, so every
    /// provider request is counted — including the one inside a tool loop.
    async fn collect(
        &mut self,
        engine: &Engine,
        session: SessionId,
        seen: &mut usize,
        phase: &'static str,
        turn: &'static str,
    ) -> Result<(), Box<dyn Error>> {
        let history = engine.history(session).await?;
        for message in history.iter().skip(*seen) {
            if message.role != Role::Assistant {
                continue;
            }
            let Some(origin) = &message.origin else {
                continue;
            };
            let usage = &origin.usage;
            let row = RequestRow {
                phase,
                turn,
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_read: usage.cache_read_tokens,
                cache_write: usage.cache_write_tokens,
                cost: reported_cost(usage),
            };
            let number = self.requests.len() + 1;
            println!(
                "  request {number} · input {} · output {} · cache read {} · cache write {} · {}",
                tokens(row.input),
                tokens(row.output),
                tokens(row.cache_read),
                tokens(row.cache_write),
                row.cost.map_or_else(
                    || "no cost reported".to_owned(),
                    |cost| format!("${cost:.6}")
                )
            );
            self.requests.push(row);
        }
        *seen = history.len();
        Ok(())
    }

    /// The bytes, request by request: the message prefix shared with the request
    /// before it, and its digest. This is the half of a cache hit the library
    /// owns.
    ///
    /// `before` seeds the comparison with another engine's calls, so the first
    /// request of a process that imported a conversation is compared against the
    /// last request of the process that exported it — which is the whole point of
    /// the last phase.
    fn audit(&mut self, phase: &'static str, captured: &Captured, before: Option<&Captured>) {
        let mut lines: Vec<(String, RequestRecord)> = Vec::new();
        if let Some(before) = before {
            let requests = before
                .lock()
                .expect("the probe lock is never held across an await")
                .clone();
            if let Some(last) = requests.last() {
                lines.push(("the other engine's last request".to_owned(), last.clone()));
            }
        }
        for (index, request) in captured
            .lock()
            .expect("the probe lock is never held across an await")
            .iter()
            .enumerate()
        {
            lines.push((format!("request {}", index + 1), request.clone()));
        }

        let mut previous: Option<(String, Vec<Value>)> = None;
        for (label, request) in lines {
            let messages = messages_of(&request.body);
            let bytes = request.bytes;
            let (shared, same, digest) = match &previous {
                None => {
                    println!("  {label}: {} message(s), {bytes} bytes", messages.len());
                    (None, None, None)
                }
                Some((before_label, previous_messages)) => {
                    let shared = messages.len().min(previous_messages.len());
                    let same = previous_messages.len() >= shared
                        && messages[..shared] == previous_messages[..shared];
                    let digest =
                        Digest::of_json(&Value::Array(messages[..shared].to_vec())).to_string();
                    println!(
                        "  {label}: {} message(s), {bytes} bytes, {shared} shared with \
                         {before_label} — {} ({digest})",
                        messages.len(),
                        if same { "byte-identical" } else { "CHANGED" }
                    );
                    (Some(shared), Some(same), Some(digest))
                }
            };
            self.audits.push(AuditRow {
                phase,
                label: label.clone(),
                messages: messages.len(),
                bytes,
                shared,
                same,
                digest,
            });
            previous = Some((label, messages));
        }
    }

    /// The closing word, whichever way the provider behaved.
    fn closing(&self) -> String {
        let hits: Vec<&RequestRow> = self
            .requests
            .iter()
            .filter(|row| row.cache_read.unwrap_or(0) > 0)
            .collect();
        if hits.is_empty() {
            return "the provider reported no cache read on any request, so no live hit is proven \
                    here. The prefixes above are still byte-identical, which is what the library \
                    promises and the provider's side of the bargain; a provider that does not \
                    report cache usage (or a prefix under its floor) cannot show one."
                .to_owned();
        }
        let tokens: u64 = hits.iter().filter_map(|row| row.cache_read).sum();
        format!(
            "the provider reported {tokens} cached token(s) across {} request(s), each one on a \
             request whose shared prefix the audit above shows byte-identical to the request \
             before it — including the resume after the approval and the first turn after the \
             import.",
            hits.len()
        )
    }
}

fn messages_of(request: &Value) -> Vec<Value> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn reported_cost(usage: &Usage) -> Option<f64> {
    usage.vendor.get("cost").and_then(Value::as_f64)
}

fn hit_rate(input: Option<u64>, read: Option<u64>) -> String {
    match (input, read) {
        (Some(input), Some(read)) => norte_bench_support::percent(read.min(input), input),
        _ => "—".to_owned(),
    }
}

/// The hit rate as a spreadsheet cell: a bare number, or empty when the
/// provider reported no counters.
fn hit_csv(input: Option<u64>, read: Option<u64>) -> String {
    match (input, read) {
        (Some(input), Some(read)) if input > 0 => format!(
            "{:.1}",
            100.0 * norte_bench_support::as_f64(read.min(input))
                / norte_bench_support::as_f64(input)
        ),
        _ => String::new(),
    }
}

// ── the report ──────────────────────────────────────────────────────────────

/// The CSV headers, one per table the run writes.
const TURN_HEADERS: &[&str] = &["phase", "turn", "elapsed"];
const REQUEST_HEADERS: &[&str] = &[
    "number",
    "phase",
    "turn",
    "input",
    "output",
    "cache_read",
    "cache_write",
    "hit",
    "cost",
];
const AUDIT_HEADERS: &[&str] = &[
    "phase", "request", "messages", "bytes", "shared", "status", "digest",
];

fn render_report(bench: &Bench, closing: &str, route: &Route, started: DateTime<Utc>) -> Report {
    let mut report = Report::new("cache-hit");
    report.paragraph(
        "What survives a turn, a tool call, a park and an export, in the provider's own numbers.",
    );

    report.section("The run");
    report.kv(&[
        (
            "started",
            started.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ),
        ("model", format!("`{}`", route.model)),
        (
            "provider",
            route
                .provider
                .clone()
                .unwrap_or_else(|| "the router decides".to_owned()),
        ),
        ("endpoint", format!("`{}`", route.endpoint)),
        (
            "pause",
            format!("{} ms before each request", route.pause_ms),
        ),
        ("phases", "normal, tool, approval, export".to_owned()),
    ]);

    report.section("Reproduce");
    report.code("sh", &reproduce(route));

    report.section("Turns");
    report.paragraph(if route.pause_ms == 0 {
        "No pause is inserted between requests; a provider cache sees exactly the pace the host \
         would produce."
    } else {
        "Every request waits `BENCH_PAUSE_MS` before it is sent, so the provider can publish the \
         prefix cache of the request before it. The wait is included in the elapsed column and is \
         protocol, not model time."
    });
    report.table(
        &["phase", "turn", "elapsed"],
        &[Align::Left, Align::Left, Align::Right],
        &turn_cells(bench),
    );

    report.section("Requests");
    report.table(
        &[
            "#",
            "phase",
            "turn",
            "input",
            "output",
            "cache read",
            "cache write",
            "hit",
            "cost",
        ],
        &[
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
        &request_cells(bench),
    );
    report.paragraph(&format!("**Bill.** {}.", bill(bench)));

    report.section("Prefix audit");
    report.table(
        &[
            "phase", "request", "messages", "bytes", "shared", "status", "digest",
        ],
        &[
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Left,
        ],
        &audit_cells(bench),
    );

    report.section("What this shows");
    report.paragraph(closing);
    report
}

fn reproduce(route: &Route) -> String {
    let mut out = String::new();
    out.push_str("export OPENROUTER_API_KEY=...\n");
    let _ = writeln!(out, "export BENCH_ENDPOINT={}", route.endpoint);
    let _ = writeln!(out, "export BENCH_MODEL={}", route.model);
    if let Some(provider) = &route.provider {
        let _ = writeln!(out, "export BENCH_PROVIDER={provider}");
    }
    let _ = writeln!(out, "export BENCH_PAUSE_MS={}", route.pause_ms);
    out.push_str("cargo run -p norte-bench-cache-hit");
    out
}

fn turn_cells(bench: &Bench) -> Vec<Vec<String>> {
    bench
        .turns
        .iter()
        .map(|row| {
            vec![
                row.phase.to_owned(),
                row.label.to_owned(),
                duration(row.elapsed),
            ]
        })
        .collect()
}

fn request_cells(bench: &Bench) -> Vec<Vec<String>> {
    bench
        .requests
        .iter()
        .enumerate()
        .map(|(index, row)| {
            vec![
                (index + 1).to_string(),
                row.phase.to_owned(),
                row.turn.to_owned(),
                tokens(row.input),
                tokens(row.output),
                tokens(row.cache_read),
                tokens(row.cache_write),
                hit_rate(row.input, row.cache_read),
                money_opt(row.cost),
            ]
        })
        .collect()
}

/// The request table as CSV: numbers stay numbers, so a spreadsheet can sum
/// them, and an absent counter is an empty cell rather than an em dash.
fn request_rows(bench: &Bench) -> Vec<Vec<String>> {
    bench
        .requests
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let number =
                |value: Option<u64>| value.map_or_else(String::new, |value| value.to_string());
            let cost =
                |value: Option<f64>| value.map_or_else(String::new, |value| format!("{value:.6}"));
            vec![
                (index + 1).to_string(),
                row.phase.to_owned(),
                row.turn.to_owned(),
                number(row.input),
                number(row.output),
                number(row.cache_read),
                number(row.cache_write),
                hit_csv(row.input, row.cache_read),
                cost(row.cost),
            ]
        })
        .collect()
}

fn audit_cells(bench: &Bench) -> Vec<Vec<String>> {
    bench
        .audits
        .iter()
        .map(|row| {
            vec![
                row.phase.to_owned(),
                row.label.clone(),
                row.messages.to_string(),
                row.bytes.to_string(),
                row.shared
                    .map_or_else(|| "—".to_owned(), |shared| shared.to_string()),
                match row.same {
                    Some(true) => "byte-identical",
                    Some(false) => "CHANGED",
                    None => "—",
                }
                .to_owned(),
                row.digest.clone().unwrap_or_else(|| "—".to_owned()),
            ]
        })
        .collect()
}

fn audit_rows(bench: &Bench) -> Vec<Vec<String>> {
    bench
        .audits
        .iter()
        .map(|row| {
            vec![
                row.phase.to_owned(),
                row.label.clone(),
                row.messages.to_string(),
                row.bytes.to_string(),
                row.shared
                    .map_or_else(String::new, |shared| shared.to_string()),
                match row.same {
                    Some(true) => "byte-identical",
                    Some(false) => "changed",
                    None => "",
                }
                .to_owned(),
                row.digest.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

fn bill(bench: &Bench) -> String {
    let prices: Vec<f64> = bench.requests.iter().filter_map(|row| row.cost).collect();
    let unpriced = bench.requests.len().saturating_sub(prices.len());
    if prices.is_empty() {
        "no request reported a `cost`".to_owned()
    } else if unpriced == 0 {
        format!(
            "${:.6} across {} request(s)",
            prices.iter().sum::<f64>(),
            prices.len()
        )
    } else {
        format!(
            "${:.6} across {} of {} request(s); {unpriced} reported no cost",
            prices.iter().sum::<f64>(),
            prices.len(),
            bench.requests.len()
        )
    }
}

// ── the prompt ──────────────────────────────────────────────────────────────

/// A long, deterministic system message: the prefix has to be past the
/// provider's caching floor before any of this is observable.
fn manual() -> String {
    let mut out = String::from(
        "You are a concise assistant. Answer in one short sentence. \
         If asked to count words, you MUST call the `word_count` tool with the text. \
         If asked to deploy, you MUST call the `deploy` tool. \
         The numbered rules below are standing instructions; each one is a rule.\n",
    );
    for rule in 1..=180 {
        writeln!(
            out,
            "Rule {rule}: instruction number {rule} stands on its own and applies to every answer; \
             it is stated here so that the conversation has a prefix long enough to be worth \
             caching, and rule {rule} is not special in any other way."
        )
        .expect("writing to a String never fails");
    }
    out
}

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

#[norte_core::tool(name = "deploy", description = "Deploy a service to an environment")]
struct Deploy;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeployArgs {
    service: String,
    environment: String,
}

#[derive(Debug, Serialize)]
struct DeployResult {
    url: String,
}

#[async_trait::async_trait]
impl FnExecutor<DeployArgs, DeployResult> for Deploy {
    async fn call(&self, args: DeployArgs) -> norte_core::Result<DeployResult> {
        Ok(DeployResult {
            url: format!("https://{}.example.test/{}", args.environment, args.service),
        })
    }
}

const ASK_A: &str = "In one sentence: what is a prompt cache?";
const ASK_B: &str = "And in one sentence: what is a cache miss?";
const COUNT_WORDS: &str = "Count the words in this exact text: one two three four.";
const DEPLOY: &str = "Deploy the service checkout to staging.";

// ── the wiring ──────────────────────────────────────────────────────────────

/// A fresh engine — its own store, its own probe — over the same model, with
/// both tools declared from the start: changing the exposed set between turns
/// would change the request for reasons that have nothing to do with caching.
fn new_engine(route: &Route) -> Result<(Engine, Captured), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&WordCount)?;
    tools.register_declarator(&Deploy)?;
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "human",
        vec![ToolName::new("deploy")?],
        "deploying touches production",
    ))]);
    let captured = new_capture();
    let model = RecordingOpenAi::new(
        route.openai(tool_capabilities())?,
        Arc::clone(&captured),
        route.pause(),
    );
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .policies(policies)
        .build()?;
    Ok((engine, captured))
}

fn tool_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bench {
        let mut bench = Bench::default();
        bench.turn("1. normal", "the turn", Duration::from_millis(12));
        bench.requests.push(RequestRow {
            phase: "1. normal",
            turn: "the turn",
            input: Some(10),
            output: Some(2),
            cache_read: Some(5),
            cache_write: Some(0),
            cost: Some(0.000_1),
        });
        bench.audits.push(AuditRow {
            phase: "1. normal",
            label: "request 1".to_owned(),
            messages: 3,
            bytes: 100,
            shared: None,
            same: None,
            digest: Some("abc".to_owned()),
        });
        bench
    }

    #[test]
    fn every_row_matches_its_header() {
        let bench = sample();
        assert!(
            turn_cells(&bench)
                .iter()
                .all(|row| row.len() == TURN_HEADERS.len())
        );
        assert!(
            request_cells(&bench)
                .iter()
                .all(|row| row.len() == REQUEST_HEADERS.len())
        );
        assert!(
            request_rows(&bench)
                .iter()
                .all(|row| row.len() == REQUEST_HEADERS.len())
        );
        assert!(
            audit_cells(&bench)
                .iter()
                .all(|row| row.len() == AUDIT_HEADERS.len())
        );
        assert!(
            audit_rows(&bench)
                .iter()
                .all(|row| row.len() == AUDIT_HEADERS.len())
        );
    }

    #[test]
    fn the_report_names_the_route_and_the_audit() {
        let bench = sample();
        let route = Route {
            model: "test/model".to_owned(),
            endpoint: "https://example.test/v1".to_owned(),
            provider: Some("test".to_owned()),
            key_env: "TEST_KEY".to_owned(),
            pause_ms: 0,
        };
        let report = render_report(&bench, "closing", &route, Utc::now()).finish();
        assert!(report.contains("test/model"));
        assert!(report.contains("Prefix audit"));
        assert!(report.contains("**Bill.**"));
    }
}
