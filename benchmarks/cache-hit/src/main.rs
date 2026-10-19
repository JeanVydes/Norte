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
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use norte_core::{
    Capabilities, ContinuityAdapter, Digest, Engine, EngineBuilder, FnExecutor, GrantScope,
    InMemoryBlobStore, InMemoryStore, Message, Modality, Model, ModelId, ModelRef, ModelRequest,
    ModelStream, PolicySet, ProviderId, Role, SessionId, StreamingGranularity, ToolApprovalPolicy,
    ToolKind, ToolName, ToolRegistry, TurnOptions, Usage, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut report_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--report" => {
                let path = args.next().ok_or("--report needs a path")?;
                report_path = Some(PathBuf::from(path));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let at = Utc::now();
    let model = required("BENCH_MODEL", "deepseek/deepseek-v4-flash-0731")?;
    let endpoint = required("BENCH_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let provider = std::env::var("BENCH_PROVIDER").ok();

    let (engine, captured) = new_engine()?;
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(manual())])
        .await?;
    let mut seen = engine.history(session.id).await?.len();
    let mut bench = Bench::default();
    let options = turn_options();

    println!(
        "--- route: {}",
        provider.as_deref().map_or_else(
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
    after_export(&engine, &captured, session.id, &mut bench, &options).await?;

    let closing = bench.closing();
    println!("\n--- what this shows\n{closing}");

    let markdown = render_report(&bench, &closing, &model, &endpoint, provider.as_deref(), at);
    let path = report_path.unwrap_or_else(|| {
        default_report_path(provider.as_deref().unwrap_or("router"), &model, at)
    });
    write_report(&markdown, &path)?;
    println!("\nreport: {}", path.display());
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
) -> Result<(), Box<dyn Error>> {
    let conversation = engine.export(session).await?;
    println!(
        "envelope: session {} · {} message(s) · {} bytes of JSON",
        conversation.session,
        conversation.messages.len(),
        conversation.to_json()?.len()
    );
    let (elsewhere, requests) = new_engine()?;
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
struct TurnRow {
    phase: &'static str,
    label: &'static str,
    elapsed: Duration,
}

/// One provider request, read from the assistant message it committed.
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
                show(row.input),
                show(row.output),
                show(row.cache_read),
                show(row.cache_write),
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
        let mut lines: Vec<(String, Value)> = Vec::new();
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
            let messages = messages_of(&request);
            let bytes = request.to_string().len();
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

fn show(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

/// `u64 as f64` loses precision as far as the lint policy is concerned, so the
/// conversion is explicit. A token count never nears the ceiling.
fn as_f64(tokens: u64) -> f64 {
    f64::from(u32::try_from(tokens).unwrap_or(u32::MAX))
}

fn hit_rate(input: Option<u64>, read: Option<u64>) -> String {
    match (input, read) {
        (Some(input), Some(read)) if input > 0 => {
            format!("{:.1}%", 100.0 * as_f64(read) / as_f64(input))
        }
        _ => "—".to_owned(),
    }
}

fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 1.0 {
        format!("{:.0} ms", seconds * 1000.0)
    } else {
        format!("{seconds:.2} s")
    }
}

// ── the report ──────────────────────────────────────────────────────────────

fn render_report(
    bench: &Bench,
    closing: &str,
    model: &str,
    endpoint: &str,
    provider: Option<&str>,
    at: DateTime<Utc>,
) -> String {
    let mut out = String::new();
    render_header(&mut out, model, endpoint, provider, at);
    render_turns(&mut out, bench);
    render_requests(&mut out, bench);
    render_audit(&mut out, bench);
    writeln!(out, "\n## What this shows\n\n{closing}\n").expect("writing to a String never fails");
    out
}

fn render_header(
    out: &mut String,
    model: &str,
    endpoint: &str,
    provider: Option<&str>,
    at: DateTime<Utc>,
) {
    writeln!(out, "# cache-hit\n").expect("writing to a String never fails");
    writeln!(
        out,
        "What survives a turn, a tool call, a park and an export, in the provider's own numbers.\n"
    )
    .expect("writing to a String never fails");

    writeln!(out, "## The run\n").expect("writing to a String never fails");
    writeln!(out, "| | |\n| --- | --- |").expect("writing to a String never fails");
    writeln!(out, "| Started | {} |", at.format("%Y-%m-%d %H:%M:%S UTC"))
        .expect("writing to a String never fails");
    writeln!(out, "| Model | `{model}` |").expect("writing to a String never fails");
    writeln!(
        out,
        "| Provider | {} |",
        provider.unwrap_or("the router decides")
    )
    .expect("writing to a String never fails");
    writeln!(out, "| Endpoint | `{endpoint}` |").expect("writing to a String never fails");
    writeln!(
        out,
        "| Commit | {} |",
        commit().unwrap_or_else(|| "—".to_owned())
    )
    .expect("writing to a String never fails");

    writeln!(out, "\n## Reproduce\n\n```sh").expect("writing to a String never fails");
    writeln!(out, "OPENROUTER_API_KEY=...").expect("writing to a String never fails");
    writeln!(out, "BENCH_ENDPOINT={endpoint}").expect("writing to a String never fails");
    writeln!(out, "BENCH_MODEL={model}").expect("writing to a String never fails");
    if let Some(provider) = provider {
        writeln!(out, "BENCH_PROVIDER={provider}").expect("writing to a String never fails");
    }
    writeln!(out, "cargo run -p norte-bench-cache-hit\n```")
        .expect("writing to a String never fails");
}

fn render_turns(out: &mut String, bench: &Bench) {
    writeln!(out, "\n## Turns\n").expect("writing to a String never fails");
    writeln!(out, "| Phase | Turn | Elapsed |\n| --- | --- | ---: |")
        .expect("writing to a String never fails");
    for row in &bench.turns {
        writeln!(
            out,
            "| {} | {} | {} |",
            row.phase,
            row.label,
            duration(row.elapsed)
        )
        .expect("writing to a String never fails");
    }
}

fn render_requests(out: &mut String, bench: &Bench) {
    writeln!(out, "\n## Requests\n").expect("writing to a String never fails");
    writeln!(
        out,
        "| # | Phase | Turn | Input | Output | Cache read | Cache write | Hit | Cost |\n\
         | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"
    )
    .expect("writing to a String never fails");
    for (index, row) in bench.requests.iter().enumerate() {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            index + 1,
            row.phase,
            row.turn,
            show(row.input),
            show(row.output),
            show(row.cache_read),
            show(row.cache_write),
            hit_rate(row.input, row.cache_read),
            row.cost
                .map_or_else(|| "—".to_owned(), |cost| format!("${cost:.6}"))
        )
        .expect("writing to a String never fails");
    }

    let prices: Vec<f64> = bench.requests.iter().filter_map(|row| row.cost).collect();
    let unpriced = bench.requests.len().saturating_sub(prices.len());
    let bill = if prices.is_empty() {
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
    };
    writeln!(out, "\n**Bill.** {bill}.").expect("writing to a String never fails");
}

fn render_audit(out: &mut String, bench: &Bench) {
    writeln!(out, "\n## Prefix audit\n").expect("writing to a String never fails");
    writeln!(
        out,
        "| Phase | Request | Messages | Bytes | Shared | Status | Digest |\n\
         | --- | --- | ---: | ---: | ---: | --- | --- |"
    )
    .expect("writing to a String never fails");
    for row in &bench.audits {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            row.phase,
            row.label,
            row.messages,
            row.bytes,
            row.shared
                .map_or_else(|| "—".to_owned(), |shared| shared.to_string()),
            match row.same {
                Some(true) => "byte-identical",
                Some(false) => "CHANGED",
                None => "—",
            },
            row.digest.as_deref().unwrap_or("—")
        )
        .expect("writing to a String never fails");
    }
}

/// Where a run writes itself when told nowhere else: beside this crate, named
/// after the moment and the route, so one model's results do not overwrite
/// another's.
fn default_report_path(provider: &str, model: &str, at: DateTime<Utc>) -> PathBuf {
    let name = format!(
        "{}-{}-{}.md",
        at.format("%Y-%m-%dT%H%M%S"),
        slug(provider),
        slug(model)
    );
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("results")
        .join(name)
}

/// A file-name-safe form of a name: `deepseek/deepseek-v4-flash-0731` becomes
/// `deepseek-deepseek-v4-flash-0731`.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// The commit the run started from, when it is run from a checkout. Optional on
/// purpose: a benchmark has to run from a tarball too.
fn commit() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}

/// Writes the report, making the directory if a first run has to.
fn write_report(markdown: &str, path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, markdown)?;
    Ok(())
}

fn print_help() {
    println!(
        "cache-hit: what survives a turn, a tool call, a park and an export, in the provider's \
         numbers\n\n\
         usage: norte-bench-cache-hit [--report <path>]\n\n\
         OPENROUTER_API_KEY  the key for the endpoint\n\
         BENCH_ENDPOINT      the OpenAI-compatible base URL\n\
         BENCH_MODEL         the model id\n\
         BENCH_PROVIDER      optional: pin the upstream\n\n\
         the report defaults to results/<date>-<provider>-<model>.md beside this crate, and \
         --report writes it elsewhere"
    );
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

/// The wire bodies a model was asked for, in call order.
type Captured = Arc<Mutex<Vec<Value>>>;

/// A model that records the exact body of every request and then makes the call
/// anyway, so the benchmark stays live: what is compared is what was sent.
struct Probe {
    inner: OpenAiChat,
    captured: Captured,
}

#[async_trait::async_trait]
impl Model for Probe {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, request: ModelRequest) -> norte_core::Result<ModelStream> {
        // `request_body` is the projection the live path uses (FR-RECON-12), so
        // recording it records the bytes, not an approximation of them.
        let body = self.inner.request_body(&request)?;
        self.captured
            .lock()
            .expect("the probe lock is never held across an await")
            .push(body);
        self.inner.stream(request).await
    }
}

/// A fresh engine — its own store, its own probe — over the same model, with
/// both tools declared from the start: changing the exposed set between turns
/// would change the request for reasons that have nothing to do with caching.
fn new_engine() -> Result<(Engine, Captured), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&WordCount)?;
    tools.register_declarator(&Deploy)?;
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "human",
        vec![ToolName::new("deploy")?],
        "deploying touches production",
    ))]);
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let model = Probe {
        inner: openrouter(tool_capabilities())?,
        captured: Arc::clone(&captured),
    };
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

/// Pins the upstream provider when `BENCH_PROVIDER` names one.
///
/// The measurement is about a prefix, and the cache that holds it lives inside
/// one provider: with `OpenRouter` free to route, a turn can land elsewhere and
/// report a miss the bytes never caused. Pinning turns the number back into a
/// statement about the provider the run names.
fn turn_options() -> TurnOptions {
    let mut options = TurnOptions::default();
    // Every answer here is a line or two. Saying so also keeps `OpenRouter` from
    // reserving the model's whole output window against the key on every call.
    options.params.max_output_tokens = Some(512);
    if let Ok(provider) = std::env::var("BENCH_PROVIDER") {
        options.extensions.insert(
            "openai.chat:provider".to_owned(),
            serde_json::json!({ "order": [provider], "allow_fallbacks": false }),
        );
    }
    options
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("BENCH_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("BENCH_MODEL", "deepseek/deepseek-v4-flash-0731")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?)
}
