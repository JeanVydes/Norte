//! What an agentic session costs: a coding agent working on an existing project,
//! with the meter on.
//!
//! `seed` writes a small, real, dependency-free crate into the scratch directory
//! — `Cargo.toml`, `src/lib.rs`, `src/config.rs`, `src/main.rs`,
//! `tests/words.rs` and `README.md` — and the three tasks are the requests a
//! coding agent actually gets on a codebase it did not write: rename a public
//! function everywhere it is used or mentioned, change what a word is and fix
//! the test that pinned the old rule, and thread a flag from the command line
//! into the config. Nothing is written from scratch, so the agent has to read
//! before it edits. It can also run the crate's own build — `cargo test
//! --offline`, and `cargo run` when a task asks what the line prints — and what
//! the compiler says comes back as a tool result, so a change that does not build
//! is something the model reads and fixes inside the turn. The run ends by
//! printing the crate it left behind, and the host then builds it on its own:
//! `cargo test` checks the agent's work instead of taking its word for it.
//!
//! **Every model request the engine made is printed**, each with the usage that
//! produced it — including the ones inside a tool loop, which a turn's outcome
//! alone would hide: the outcome carries the last answer, while every iteration
//! committed an assistant message of its own.
//!
//! A task that runs out of budget is measured too: the loop gets 32 requests
//! rather than a chat turn's 16, and a task that fails anyway prints the
//! requests it burned, because the provider billed every one of them.
//!
//! The engine runs no command — `read`, `write` and `edit` are the whole tool
//! set — so nothing the model says about its own work has been tested by the
//! time it says it. The host closes that gap: the run ends by building the crate
//! the agent left, running its tests, and running the command line the tasks
//! asked for, with the verdict printed beside the agent's answer.
//!
//! The money is the provider's own number, and not one this benchmark works out.
//! `Usage` carries the token counters (input, output, reasoning, cache read,
//! cache write, audio), an extension map for the counters with no neutral field,
//! and `vendor`: whatever the payload reported and that vocabulary does not
//! name, kept verbatim. An `OpenAI`-compatible response reports what it billed in
//! `usage.cost`, and the meter sums *that* field per request. No price table
//! lives here on purpose: which upstream answers a request is the router's
//! decision, one request to the next, so a rate written down here would be the
//! list price of a route that may not be the one that served it.
//!
//! What the benchmark adds is the tally: how many requests a session took, the
//! tokens in and out, how much of the input came from the provider's cache — and
//! the count of requests is the number that makes an agent cheap or expensive,
//! because every one of them re-sends a transcript that grew by the last result.
//!
//! The cache counter is worth reading even so, and it is the one place token
//! arithmetic still bites: an `OpenAI`-compatible provider counts what it read
//! from the cache *inside* `prompt_tokens`, so the hit rate is
//! `cache_read / input` and those tokens are already in the total. The counters
//! are printed beside the bill, never instead of it, and what the hit was worth
//! is in the provider's number rather than in a multiplication done here.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
//! BENCH_PROVIDER=DeepInfra \
//! cargo run -p rustychain-bench-agent-cost
//! ```
//!
//! `BENCH_PROVIDER` pins the upstream so that two runs stay comparable; it is
//! a routing hint and never a price. Without it the router picks, and the field
//! the response carries says what that route charged.
//!
//! A run leaves a report behind: the same figures, per task and per request,
//! written as markdown under `results/` next to the ones before it, so what a
//! model cost on a given day can be read back without replaying the run.
//! `--report <path>` writes it elsewhere.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rustychain_core::{
    Capabilities, Decision, Engine, EngineBuilder, EngineConfig, EngineEvent, InMemoryBlobStore,
    InMemoryStore, Message, Modality, ModelId, ModelRef, Part, Policy, PolicySet, PolicyView,
    ProviderId, SessionId, StreamDelta, StreamingGranularity, ToolKind, ToolRegistry, TurnOptions,
    Usage, WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};
use rustychain_tools::{CommandTool, EditTool, ReadTool, WriteTool};
use serde_json::{Value, json};

/// The workflow every task follows: read what is on disk, change it in place,
/// and keep the crate consistent.
const SYSTEM: &str = "\
You are a coding agent working in a scratch directory that already holds the \
`words` crate. Use the tools, never describe what you would do. For each task: \
(1) `read` every file you are about to change, before you change it; (2) `edit` \
the files in place — do not replace a whole file with `write` when the task is a \
change; (3) update every call site, every test and every mention in the README, \
so nothing in the crate still refers to the old shape; (4) before you answer, \
build what you left: run `cargo test --offline` with the `command` tool, and \
run the crate with it whenever the task asks what it prints — a compiler error \
is a tool result you can read and fix, and an answer about code nothing has run \
is a guess; (5) answer in one short line. Every path is relative to the scratch \
directory: `src/lib.rs`, `tests/words.rs`, `README.md`.";

/// Three requests of the kind a coding agent gets on someone else's code.
const TASKS: [&str; 3] = [
    "Rename the public function `head_words` to `words_up_to` everywhere: its definition in \
     `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list \
     in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you \
     changed.",
    "A word currently keeps the punctuation it was written with, so `--verbose \"the sky, is \
     blue.\"` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in \
     both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — \
     its name is wrong now too. Answer with the report that line prints.",
    "Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that \
     nothing sets from the command line, so `--verbose --limit 2` should report only the first \
     two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer \
     with the code you added.",
];

/// The requests a task may spend before the loop gives up. Higher than a chat
/// turn's 16 on purpose: a coding task reads, edits and reads back, and a request
/// that runs out of budget is one the provider already billed.
const BUDGET: usize = 32;

/// What the provider field says when the run did not pin one.
const ROUTER_PICKS: &str = "whoever the router picks";

/// The crate `seed` writes before the agent runs: real files, already written,
/// which every task has to read before it can change anything. They are here
/// verbatim so the code the model sees is the code a reader sees.
#[allow(clippy::needless_raw_string_hashes)]
const PROJECT: [(&str, &str); 6] = [
    (
        "Cargo.toml",
        r#"[package]
name = "words"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    ),
    (
        "src/lib.rs",
        r#"//! Counting the words of a line of text.

pub mod config;

pub use config::Config;

/// The number of words in `text`, separated by whitespace.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The first `limit` words of `text`, or all of them when `limit` is zero.
pub fn head_words(text: &str, limit: usize) -> Vec<&str> {
    let words = text.split_whitespace();
    if limit == 0 {
        words.collect()
    } else {
        words.take(limit).collect()
    }
}

/// The one-line report `main` prints for `text` under `config`.
pub fn report(text: &str, config: &Config) -> String {
    let count = count_words(text);
    if config.verbose {
        format!("{count} words: {}", head_words(text, config.limit).join(", "))
    } else {
        format!("{count} words")
    }
}
"#,
    ),
    (
        "src/config.rs",
        r#"/// How the command line reports a count.
#[derive(Debug)]
pub struct Config {
    /// Print the words themselves, not only the count.
    pub verbose: bool,
    /// Report at most this many words; zero reports every word.
    pub limit: usize,
}
"#,
    ),
    (
        "src/main.rs",
        r#"use std::env;

use words::{report, Config};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let verbose = args.iter().any(|arg| arg == "--verbose");
    let text = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    println!("{}", report(&text, &Config { verbose, limit: 0 }));
}
"#,
    ),
    (
        "tests/words.rs",
        r#"use words::{count_words, head_words, report, Config};

#[test]
fn counts_words_separated_by_spaces() {
    assert_eq!(count_words("the sky is blue"), 4);
}

#[test]
fn punctuation_belongs_to_the_word_it_is_written_on() {
    assert_eq!(head_words("the sky, is blue.", 0), ["the", "sky,", "is", "blue."]);
}

#[test]
fn reports_the_first_words_when_verbose() {
    let config = Config { verbose: true, limit: 2 };
    assert_eq!(report("the sky is blue", &config), "4 words: the, sky");
}
"#,
    ),
    (
        "README.md",
        r#"# words

Counts the words of the line it is given.

```sh
cargo run -- "the sky is blue"             # 4 words
cargo run -- --verbose "the sky is blue"   # 4 words: the, sky, is, blue
```

## API

- `count_words(text) -> usize` — how many words the line holds.
- `head_words(text, limit) -> Vec<&str>` — the first `limit` words, or all of them when `limit` is zero.
- `report(text, config) -> String` — the one-line report `main` prints.
"#,
    ),
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut destination = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => {
                let path = args.next().ok_or("--report needs a path")?;
                destination = Some(PathBuf::from(path));
            }
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}`; try --help").into()),
        }
    }

    let started = Utc::now();
    let model = required("BENCH_MODEL", "deepseek/deepseek-v4-flash-0731")?;
    let endpoint = required("BENCH_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let provider = std::env::var("BENCH_PROVIDER").unwrap_or_else(|_| ROUTER_PICKS.to_owned());

    let root = scratch()?;
    seed(&root)?;
    let (engine, options) = engine(&root, &endpoint, &model)?;

    println!("scratch:  {}", root.display());
    println!(
        "seeded:   {}",
        PROJECT
            .iter()
            .map(|(path, _)| *path)
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("provider: {provider}");
    println!(
        "money:    the `usage.cost` every response reports, summed; the benchmark applies no price \
         of its own\n"
    );

    let mut ledger = Ledger::default();
    let mut results = Vec::new();
    for (index, task) in TASKS.iter().enumerate() {
        println!("--- task {}: {task}", index + 1);
        let session = engine.create().await?;
        engine
            .append_history(session.id, vec![Message::system(SYSTEM)])
            .await?;

        // The turn is a stream, and it is drained rather than finished: a turn
        // that runs out of budget ends in an `Err`, but every request it made is
        // already committed and already billed, so the events are read either
        // way and only the last line says whether the agent got there.
        let mut screen = Screen::default();
        let mut stream = engine
            .turn(session.id)
            .input(Message::user(*task))
            .options(options.clone())
            .start();
        while let Some(event) = stream.next().await {
            let event = event?;
            screen.event(&engine, session.id, &event).await?;
        }
        let outcome = stream.finish().await;

        // The last request has no successor to bill it, so the turn settles here.
        screen.settle(&engine, session.id).await?;
        let tally = screen.task;
        match outcome {
            Ok(outcome) => println!(
                "  turn:     {} part(s) committed, phase {:?}; the text above is what the model \
                 said, in order, and not a summary of it",
                outcome
                    .message
                    .as_ref()
                    .map_or(0, |message| message.parts.len()),
                outcome.session.phase
            ),
            Err(error) => {
                println!("  failed:   {error} — the requests above are what it cost to find out");
            }
        }
        println!("  task:     {}\n", tally.line());
        ledger.add(&tally);
        results.push(tally);
    }

    closing(&ledger);

    // The evidence, not the model's word: the crate as the agent left it.
    println!("\n--- the crate as the agent left it");
    print_tree(&root);
    let verdict = verify(&root);

    // The run written down, so it can be read back and compared with the next.
    let destination =
        destination.unwrap_or_else(|| default_report_path(&provider, &model, started));
    let markdown = Report {
        started,
        model: &model,
        provider: &provider,
        endpoint: &endpoint,
        commit: commit(),
        budget: BUDGET,
        tasks: &results,
        verdict: &verdict,
        scratch: &root,
    }
    .render();
    write_report(&markdown, &destination)?;
    println!("\n--- report: {}", destination.display());
    Ok(())
}

/// What the benchmark takes, for a reader who typed the crate's name and nothing
/// else.
fn usage() {
    println!(
        "agent-cost — what a coding agent costs, in the provider's own numbers\n\n\
         usage: rustychain-bench-agent-cost [--report <path>]\n\n\
         the report defaults to results/<date>-<provider>-<model>.md beside this crate; \
         `--report` writes it elsewhere\n\n\
         environment:\n  \
         OPENROUTER_API_KEY  the key for the endpoint (required)\n  \
         BENCH_ENDPOINT      the OpenAI-compatible base url (required)\n  \
         BENCH_MODEL         the model id (required)\n  \
         BENCH_PROVIDER      pins the upstream through the router, so two runs stay comparable \
         (optional)"
    );
}

/// The host's independent audit, and not the agent's claim.
///
/// The agent can run these same commands through the `command` tool, and should:
/// the compiler's complaints are tool results it gets to fix. This is the second
/// check, the one the agent does not control — the crate has to compile, its
/// tests have to pass, and the command line the tasks named has to print what the
/// task said it would, judged after the turn is over. A toolchain that is not
/// there is said out loud rather than passed off as a pass. The verdicts come
/// back as well as out, because they belong in the report too.
fn verify(root: &Path) -> Verdict {
    println!("\n--- the host checks the agent's claim");

    let tests = match cargo(root, &["test", "--quiet", "--offline"]) {
        None => Check::new("cargo test", "not run — there is no `cargo` on PATH"),
        Some((true, stdout, _)) => {
            let results = stdout
                .lines()
                .filter(|line| line.contains("test result"))
                .map(|line| line.trim().to_owned())
                .collect::<Vec<_>>();
            if results.is_empty() {
                Check::new("cargo test", "the crate builds")
            } else {
                Check::with_detail(
                    "cargo test",
                    "the crate builds and every test passes",
                    results,
                )
            }
        }
        Some((false, stdout, stderr)) => Check::with_detail(
            "cargo test",
            "FAILED",
            stdout
                .lines()
                .chain(stderr.lines())
                .filter(|line| line.contains("test result") || line.contains("error"))
                .take(6)
                .map(|line| line.trim().to_owned())
                .collect(),
        ),
    };

    let expected = "4 words: the, sky";
    let line = [
        "run",
        "--quiet",
        "--offline",
        "--",
        "--verbose",
        "--limit",
        "2",
        "the sky, is blue.",
    ];
    let printed = match cargo(root, &line) {
        None => Check::new(
            "cargo run -- --verbose --limit 2 \"the sky, is blue.\"",
            "not run — there is no `cargo` on PATH",
        ),
        Some((ok, stdout, stderr)) => {
            let got = stdout.lines().next().unwrap_or("").trim();
            let verdict = match (ok, got == expected) {
                (_, true) => format!("prints `{expected}`"),
                (true, false) => format!("prints `{got}` and not `{expected}`"),
                (false, false) => format!(
                    "failed: {}",
                    stderr
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("no message")
                        .trim()
                ),
            };
            Check::new(
                "cargo run -- --verbose --limit 2 \"the sky, is blue.\"",
                &verdict,
            )
        }
    };

    let verdict = Verdict {
        checks: vec![tests, printed],
    };
    for check in &verdict.checks {
        println!("{}: {}", check.what, check.verdict);
        for line in &check.detail {
            println!("  {line}");
        }
    }
    verdict
}

/// One command the host ran after the turn, and what it said.
struct Check {
    /// The command, as a line a reader could repeat.
    what: String,
    /// The one-line verdict.
    verdict: String,
    /// The lines worth keeping as evidence.
    detail: Vec<String>,
}

impl Check {
    fn new(what: &str, verdict: &str) -> Self {
        Self {
            what: what.to_owned(),
            verdict: verdict.to_owned(),
            detail: Vec::new(),
        }
    }

    fn with_detail(what: &str, verdict: &str, detail: Vec<String>) -> Self {
        Self {
            what: what.to_owned(),
            verdict: verdict.to_owned(),
            detail,
        }
    }
}

/// What the host found, kept for the report.
#[derive(Default)]
struct Verdict {
    checks: Vec<Check>,
}

/// Runs `cargo` in the scratch directory, offline: a crate with no dependencies
/// needs nothing from a registry. The host's tool, not the engine's — no policy
/// sees this process and no model asks for it.
fn cargo(root: &Path, args: &[&str]) -> Option<(bool, String, String)> {
    let output = std::process::Command::new("cargo")
        .args(args)
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        // The check must not land in the workspace's target directory, or it
        // would contend for a build lock the benchmark's own invocation holds.
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .ok()?;
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Writes the crate the agent is handed, so every run starts from the same
/// files and the only thing that changes them is the agent.
fn seed(root: &Path) -> Result<(), Box<dyn Error>> {
    for (path, contents) in PROJECT {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, contents)?;
    }
    Ok(())
}

/// Prints every file under `root`, in path order.
fn print_tree(root: &Path) {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files.sort();
    for file in files {
        let relative = file.strip_prefix(root).unwrap_or(&file);
        match std::fs::read_to_string(&file) {
            Ok(text) => println!("--- {}\n{text}", relative.display()),
            Err(_) => println!("--- {} (not text)", relative.display()),
        }
    }
}

fn collect_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, into);
        } else {
            into.push(path);
        }
    }
}

/// What the terminal shows while a turn runs.
///
/// The event stream carries what the model says as it says it: reasoning, text,
/// and each call when it starts and when it finishes. A message can hold text and
/// a call at once, so the text is shown where it lands — it is narration, and the
/// turn is not over while a call is pending.
///
/// What the stream does not carry is the call itself once settled, the result the
/// tool produced, or any usage. Those are read from history at the moment they
/// commit: the next request is about to be built out of them, which is exactly
/// when the reader should see the bytes the model is being handed and what they
/// cost. A result is printed and not only the call because "the agent read the
/// file" should be something the reader watched happen, not something the benchmark
/// asserts.
#[derive(Default)]
struct Screen {
    /// Requests reconstructed so far in this turn, which is the iteration.
    iteration: u64,
    /// Whether a streamed line is open, so a marker starts on its own line.
    open: bool,
    /// Whether the reasoning prefix was written for the current request.
    thinking: bool,
    /// Messages of this session already billed.
    billed: usize,
    /// The task's tally, kept live.
    task: Task,
}

impl Screen {
    /// Renders one event, billing the previous iteration as the next one opens.
    async fn event(
        &mut self,
        engine: &Engine,
        session: SessionId,
        event: &EngineEvent,
    ) -> Result<(), Box<dyn Error>> {
        match event {
            EngineEvent::RequestReconstructed {
                digest,
                cache_boundaries,
                tools,
                ..
            } => {
                self.settle(engine, session).await?;
                self.break_line();
                self.iteration += 1;
                self.thinking = false;
                println!(
                    "  ── iteration {} · {} exposed tool(s) · {} cache boundary(ies) · digest {}",
                    self.iteration,
                    tools.len(),
                    cache_boundaries.len(),
                    &digest.to_hex()[..8]
                );
            }
            EngineEvent::StreamDelta { delta, .. } => self.delta(delta),
            EngineEvent::ToolCallStarted { tool, .. } => {
                self.break_line();
                println!("  ▶ {tool} …");
            }
            EngineEvent::ToolCallFinished {
                tool,
                is_error,
                duration_ms,
                ..
            } => {
                self.break_line();
                let status = if *is_error { "failed" } else { "ok" };
                println!("  ✓ {tool} {status} in {duration_ms} ms");
                self.task.calls.push(Call {
                    name: tool.to_string(),
                    is_error: *is_error,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Bills every request committed since the last look, and prints it, along
    /// with the calls and the results that came with it.
    ///
    /// The stream does not carry them: the wire closes a tool call at the end of
    /// the message, so the accumulator hands it back from `finish()` instead of
    /// emitting a completed part, and a tool result never goes through the
    /// stream at all. History is where both land, and the moment they land is
    /// the moment the next request is about to be built out of them — the moment
    /// worth showing.
    async fn settle(&mut self, engine: &Engine, session: SessionId) -> Result<(), Box<dyn Error>> {
        let history = engine.history(session).await?;
        for message in history.iter().skip(self.billed) {
            if let Some(usage) = message.origin.as_ref().map(|origin| &origin.usage) {
                let line = self.task.count(usage);
                self.break_line();
                println!("  $ request {}: {line}", self.task.requests);
            }
            for part in &message.parts {
                match part {
                    Part::ToolCall(call) => {
                        self.break_line();
                        println!(
                            "  ⏺ {} ({})",
                            call.name,
                            preview(&call.arguments.to_string(), 160)
                        );
                    }
                    Part::ToolResult(result) => {
                        self.break_line();
                        let text = result
                            .parts
                            .iter()
                            .filter_map(Part::as_text)
                            .collect::<Vec<_>>()
                            .join(" ");
                        let mark = if result.is_error { "!" } else { "↳" };
                        println!("  {mark} {}", excerpt(&text, 8, 700));
                    }
                    _ => {}
                }
            }
        }
        self.billed = history.len();
        Ok(())
    }

    /// Streams a delta: what the model says, and what it is thinking, as it
    /// arrives. The arguments of a call are not dumped here: they are JSON, they
    /// can be whole blocks of source, and the settled call prints them once with
    /// a bound. A message may carry text and a call at the same time, so the
    /// words the model says before the call are narration, not the answer — they
    /// are shown when they land and the line is closed before the call's marker.
    fn delta(&mut self, delta: &StreamDelta) {
        match delta {
            StreamDelta::Text { text, .. } | StreamDelta::Refusal { text, .. } => {
                print!("{text}");
                self.open = true;
            }
            StreamDelta::Reasoning { text, .. } => {
                if !self.thinking {
                    self.break_line();
                    print!("  … thinking: ");
                    self.thinking = true;
                }
                print!("{text}");
                self.open = true;
            }
            _ => {}
        }
        let _ = io::stdout().flush();
    }

    /// Ends a streamed line, so the next marker is not glued to a token.
    fn break_line(&mut self) {
        if self.open {
            println!();
            self.open = false;
        }
    }
}

/// A one-line preview, so a call's arguments never flood the screen.
fn preview(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        return compact;
    }
    let mut out = compact.chars().take(max).collect::<String>();
    out.push('…');
    out
}

/// The first lines of a tool result: enough to see the bytes the model was
/// handed, and a count where the rest was left out.
fn excerpt(text: &str, max_lines: usize, max_chars: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    for (index, line) in text.lines().enumerate() {
        if index >= max_lines || (index > 0 && shown >= max_chars) {
            let _ = write!(
                out,
                "\n      … {} more line(s), {} byte(s) in total",
                text.lines().count().saturating_sub(index),
                text.len()
            );
            break;
        }
        let _ = write!(out, "\n      │ {line}");
        shown += line.len();
    }
    if out.is_empty() {
        out.push_str("(empty)");
    }
    out
}

/// One task's tally: the counters the provider reported, and the money it
/// reported for them. The requests and the calls are kept as well, in order,
/// because a total is not an audit: the report wants to show where it came from.
#[derive(Default)]
struct Task {
    requests: u64,
    input: u64,
    cached: u64,
    output: u64,
    /// The requests whose payload carried no `cost` at all.
    unpriced: u64,
    /// The vendor's own `usage.cost`, summed over the requests that carried one.
    reported: Option<f64>,
    /// Every request, as the provider reported it.
    rows: Vec<Row>,
    /// Every tool call the turn made.
    calls: Vec<Call>,
}

/// One request's counters, kept for the report.
struct Row {
    request: u64,
    input: u64,
    cached: u64,
    output: u64,
    cost: Option<f64>,
}

/// One tool call, kept for the report.
struct Call {
    name: String,
    is_error: bool,
}

impl Task {
    /// The price the payload itself carries, if it carries one.
    ///
    /// `cost` is `OpenAI`-compatible, and it reaches here untouched: it survives
    /// the parse in `Usage::vendor`, because the vocabulary does not name it. The
    /// benchmark reads that field; it never writes a price of its own.
    fn reported_cost(usage: &Usage) -> Option<f64> {
        usage.vendor.get("cost").and_then(Value::as_f64)
    }

    /// Folds one request in and returns the line for it alone.
    fn count(&mut self, usage: &Usage) -> String {
        let input = usage.input_tokens.unwrap_or(0);
        let cached = usage.cache_read_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        self.requests += 1;
        self.input += input;
        self.cached += cached;
        self.output += output;
        let reported = Self::reported_cost(usage);
        match reported {
            Some(reported) => *self.reported.get_or_insert(0.0) += reported,
            None => self.unpriced += 1,
        }
        self.rows.push(Row {
            request: self.requests,
            input,
            cached,
            output,
            cost: reported,
        });
        let money = reported.map_or_else(
            || " · no `cost` in the payload".to_owned(),
            |reported| format!(" · ${reported:.6}"),
        );
        format!(
            "input {input} · output {output} · cache read {cached}{}{money}",
            hit_of(cached, input)
        )
    }

    /// What the provider billed for the task, and what it did not say.
    fn money(&self) -> String {
        match (self.reported, self.unpriced) {
            (Some(reported), 0) => format!(" · ${reported:.6} billed"),
            (Some(reported), unpriced) => format!(
                " · ${reported:.6} billed over {} of {} request(s)",
                self.requests - unpriced,
                self.requests
            ),
            (None, _) => " · no request reported a `cost`".to_owned(),
        }
    }

    fn line(&self) -> String {
        format!(
            "{} request(s) · input {} · output {} · cache read {}{}{}",
            self.requests,
            self.input,
            self.output,
            self.cached,
            hit_of(self.cached, self.input),
            self.money()
        )
    }
}

/// The session, summed.
#[derive(Default)]
struct Ledger {
    tasks: u64,
    requests: u64,
    input: u64,
    cached: u64,
    output: u64,
    unpriced: u64,
    reported: Option<f64>,
}

impl Ledger {
    fn add(&mut self, task: &Task) {
        self.tasks += 1;
        self.requests += task.requests;
        self.input += task.input;
        self.cached += task.cached;
        self.output += task.output;
        self.unpriced += task.unpriced;
        if let Some(reported) = task.reported {
            *self.reported.get_or_insert(0.0) += reported;
        }
    }
}

/// The closing word: what one task costs, and what a hundred of them would.
fn closing(ledger: &Ledger) {
    println!("--- what this shows");
    println!(
        "{} task(s), {} model request(s): input {} · output {} · cache read {}",
        ledger.tasks, ledger.requests, ledger.input, ledger.output, ledger.cached
    );
    let tasks = as_f64(ledger.tasks.max(1));
    match ledger.reported {
        Some(reported) => {
            println!(
                "the provider's own `usage.cost`, summed over {} of {} request(s): ${reported:.6}, \
                 ${:.6} per task, ${:.4} for a hundred tasks",
                ledger.requests - ledger.unpriced,
                ledger.requests,
                reported / tasks,
                reported / tasks * 100.0
            );
            if ledger.unpriced > 0 {
                println!(
                    "{} request(s) reported no `cost`, so the bill above is short by whatever \
                     they were charged",
                    ledger.unpriced
                );
            }
        }
        None => println!(
            "no response carried a `cost`, so the counters above are the whole statement: this \
             wire reports usage without a price, and a benchmark cannot invent one"
        ),
    }
    if let Some(hit) = hit_rate(ledger.cached, ledger.input) {
        println!(
            "cache hit: {hit:.1} % of the input ({} of {} tokens came from the provider's cache). \
             A read is counted inside `input`, so it is already part of the bill above; what the \
             hit was worth is in that number, not in a rate applied here.",
            ledger.cached, ledger.input
        );
    }
    println!(
        "every figure is the provider's, per request: a transcript that grows with each tool \
         result is what an agent's bill is made of, and the request count is where it shows"
    );
}

/// The run written down: the figures the terminal showed, as markdown, so a
/// result can be read back — and compared with the next run — without replaying
/// it. It is written by the run that produced it, which is the only thing that
/// can be trusted to describe what happened.
struct Report<'a> {
    started: DateTime<Utc>,
    model: &'a str,
    provider: &'a str,
    endpoint: &'a str,
    commit: Option<String>,
    budget: usize,
    tasks: &'a [Task],
    verdict: &'a Verdict,
    scratch: &'a Path,
}

impl Report<'_> {
    /// The whole document, one section at a time: a report read by a person and
    /// by the next run, so the sections are assembled rather than printed.
    fn render(&self) -> String {
        let mut ledger = Ledger::default();
        for task in self.tasks {
            ledger.add(task);
        }
        [
            self.heading(),
            self.run(),
            self.reproduce(),
            self.tasks_table(&ledger),
            Self::asked(),
            self.calls(),
            Self::bill(&ledger),
            self.check(),
            self.requests(),
        ]
        .concat()
    }

    /// The title, what the run is, and what the figures are.
    fn heading(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# agent-cost — `{}` @ {}\n", self.model, self.provider);
        let _ = writeln!(
            out,
            "A coding agent on a crate it did not write: rename a public function, change what a \
             word is and fix the test that pinned the old rule, thread a flag from the command \
             line. Nothing is written from scratch, so the agent has to read before it edits, and \
             it can build the crate with the `command` tool — a compiler error is a tool result it \
             reads and fixes inside the turn.\n"
        );
        let _ = writeln!(
            out,
            "Every figure is the provider's own: the `usage.cost` each response reported, summed, \
             and the token counters it reported. No rate is applied here, so a request whose \
             payload carried no `cost` is counted and named as such rather than priced by this \
             file.\n"
        );
        out
    }

    /// What was run: the route, the budget, the commit.
    fn run(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "## The run\n");
        let _ = writeln!(out, "| | |");
        let _ = writeln!(out, "| --- | --- |");
        let _ = writeln!(
            out,
            "| when | {} |",
            self.started.format("%Y-%m-%d %H:%M:%S UTC")
        );
        let _ = writeln!(out, "| model | `{}` |", self.model);
        let _ = writeln!(out, "| provider | {} |", self.provider);
        let _ = writeln!(out, "| endpoint | `{}` |", self.endpoint);
        let _ = writeln!(out, "| rustychain | {} |", env!("CARGO_PKG_VERSION"));
        if let Some(commit) = &self.commit {
            let _ = writeln!(out, "| commit | `{commit}` |");
        }
        let _ = writeln!(
            out,
            "| budget | {} request(s) per task, {} task(s) |",
            self.budget,
            self.tasks.len()
        );
        let _ = writeln!(out, "| scratch | `{}` |", self.scratch.display());
        out
    }

    /// The environment, as a block that can be pasted.
    fn reproduce(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## Reproduce\n");
        let _ = writeln!(out, "```sh");
        let _ = writeln!(out, "export OPENROUTER_API_KEY=...");
        let _ = writeln!(out, "export BENCH_ENDPOINT={}", self.endpoint);
        let _ = writeln!(out, "export BENCH_MODEL={}", self.model);
        if self.provider != ROUTER_PICKS {
            let _ = writeln!(out, "export BENCH_PROVIDER={}", self.provider);
        }
        let _ = writeln!(out, "cargo run -p rustychain-bench-agent-cost");
        let _ = writeln!(out, "```");
        out
    }

    /// One row per task, and the total.
    fn tasks_table(&self, ledger: &Ledger) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## Tasks\n");
        let _ = writeln!(
            out,
            "| # | requests | input | cache read | hit | output | cost (USD) |"
        );
        let _ = writeln!(out, "| ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
        for (index, task) in self.tasks.iter().enumerate() {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                index + 1,
                task.requests,
                task.input,
                task.cached,
                hit_cell(task.cached, task.input),
                task.output,
                money_cell(task.reported)
            );
        }
        let _ = writeln!(
            out,
            "| **all** | {} | {} | {} | {} | {} | {} |",
            ledger.requests,
            ledger.input,
            ledger.cached,
            hit_cell(ledger.cached, ledger.input),
            ledger.output,
            money_cell(ledger.reported)
        );
        out
    }

    /// The tasks in full, so the figures above have something to stand on.
    fn asked() -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n### What the tasks asked\n");
        for (index, task) in TASKS.iter().enumerate() {
            let _ = writeln!(out, "{}. {task}", index + 1);
        }
        out
    }

    /// What the agent reached for, and what it cost to be told no.
    fn calls(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## Tool calls\n");
        let mut all_calls = 0_u64;
        let mut all_failed = 0_u64;
        for (index, task) in self.tasks.iter().enumerate() {
            let failed = task.calls.iter().filter(|call| call.is_error).count();
            all_calls += task.calls.len() as u64;
            all_failed += failed as u64;
            let _ = writeln!(
                out,
                "- **task {}**: {} call(s){}",
                index + 1,
                task.calls.len(),
                by_tool(&task.calls)
            );
        }
        let _ = writeln!(
            out,
            "\n{all_calls} call(s) in all, {all_failed} refused or failed. A call that fails is \
             billed like any other request, because the round that carried it was paid for before \
             the tool ever ran, which is why it is counted here."
        );
        out
    }

    /// The one figure the benchmark is about, from the provider's own field.
    fn bill(ledger: &Ledger) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## The bill\n");
        let tasks = as_f64(ledger.tasks.max(1));
        match ledger.reported {
            Some(reported) => {
                let _ = writeln!(
                    out,
                    "- the provider's `usage.cost`, summed over {} of {} request(s): \
                     **${reported:.6}**",
                    ledger.requests - ledger.unpriced,
                    ledger.requests
                );
                let _ = writeln!(
                    out,
                    "- per task: ${:.6} · per request: ${:.6} · a hundred tasks: ${:.4}",
                    reported / tasks,
                    reported / as_f64(ledger.requests.max(1)),
                    reported / tasks * 100.0
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "- no response carried a `cost`: this wire reports usage without a price, and a \
                     benchmark cannot invent one"
                );
            }
        }
        if ledger.unpriced > 0 {
            let _ = writeln!(
                out,
                "- {} request(s) reported no `cost`, so the total is short by whatever they were \
                 charged",
                ledger.unpriced
            );
        }
        if let Some(hit) = hit_rate(ledger.cached, ledger.input) {
            let _ = writeln!(
                out,
                "- cache hit: {hit:.1} % of the input ({} of {} tokens). A read is counted inside \
                 `input` on this wire, so it is already part of the total above; what the hit was \
                 worth is in that number, not in a rate applied here.",
                ledger.cached, ledger.input
            );
        }
        out
    }

    /// The host's audit, written down beside the agent's answer.
    fn check(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## The host's check\n");
        for check in &self.verdict.checks {
            let _ = writeln!(out, "- **{}**: {}", check.what, check.verdict);
            for line in &check.detail {
                let _ = writeln!(out, "  - `{line}`");
            }
        }
        out
    }

    /// Every request, in order: the audit trail behind the totals.
    fn requests(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\n## Requests\n");
        let _ = writeln!(
            out,
            "Every model request the engine made, in order, with the usage the provider reported \
             for each. A turn's outcome hides these: the transcript is re-sent and re-billed once \
             per iteration, which is what an agent's bill is made of.\n"
        );
        let _ = writeln!(
            out,
            "| task | request | input | cache read | output | cost (USD) |"
        );
        let _ = writeln!(out, "| ---: | ---: | ---: | ---: | ---: | ---: |");
        for (index, task) in self.tasks.iter().enumerate() {
            for row in &task.rows {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} |",
                    index + 1,
                    row.request,
                    row.input,
                    row.cached,
                    row.output,
                    money_cell(row.cost)
                );
            }
        }
        out
    }
}

/// The tool calls of one task, said by name: `read` ×4, `edit` ×3, `command` ×2
/// (1 failed).
fn by_tool(calls: &[Call]) -> String {
    if calls.is_empty() {
        return " — none".to_owned();
    }
    let mut tally: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for call in calls {
        let entry = tally.entry(call.name.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if call.is_error {
            entry.1 += 1;
        }
    }
    let listed = tally
        .iter()
        .map(|(name, (count, failed))| {
            if *failed > 0 {
                format!("`{name}` ×{count} ({failed} failed)")
            } else {
                format!("`{name}` ×{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" — {listed}")
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

/// `u64 as f64` loses precision as far as the lint policy is concerned, so the
/// conversion is explicit. A token count never nears the ceiling.
fn as_f64(tokens: u64) -> f64 {
    f64::from(u32::try_from(tokens).unwrap_or(u32::MAX))
}

/// How much of the input the provider read from its own cache.
///
/// The read counters are a subset of `input` on every `OpenAI`-compatible wire,
/// so this is the hit rate the provider reports and not a rate of the engine's
/// making; a provider that never reports one leaves this absent.
fn hit_rate(cached: u64, input: u64) -> Option<f64> {
    (input > 0).then(|| 100.0 * as_f64(cached.min(input)) / as_f64(input))
}

/// The hit rate as a suffix for a token line, or nothing when there is none.
fn hit_of(cached: u64, input: u64) -> String {
    hit_rate(cached, input)
        .map(|hit| format!(" ({hit:.1} % of input)"))
        .unwrap_or_default()
}

/// A hit rate as a table cell, or an em dash where the provider reported none.
fn hit_cell(cached: u64, input: u64) -> String {
    hit_rate(cached, input).map_or_else(|| "—".to_owned(), |hit| format!("{hit:.1} %"))
}

/// A reported cost as a table cell, or an em dash where the payload carried
/// none. The currency is the provider's; the column says USD because that is what
/// `OpenRouter` reports.
fn money_cell(reported: Option<f64>) -> String {
    reported.map_or_else(|| "—".to_owned(), |reported| format!("{reported:.6}"))
}

/// Pins the upstream provider when `COOKBOOK_PROVIDER` names one.
///
/// Not for the price — the response carries that. Pinning is what makes two runs
/// comparable: with `OpenRouter` free to route, "the model" is a different
/// endpoint from one request to the next, and the timings and the cache behaviour
/// move with it.
fn turn_options() -> TurnOptions {
    let mut options = TurnOptions::default();
    // Larger than a one-line answer needs: an `edit` call carries the block it
    // replaces and the block it writes, and a 512-token ceiling truncates the
    // arguments of a multi-line change mid-JSON.
    options.params.max_output_tokens = Some(1024);
    if let Ok(provider) = std::env::var("COOKBOOK_PROVIDER") {
        options.extensions.insert(
            "openai.chat:provider".to_owned(),
            json!({ "order": [provider], "allow_fallbacks": false }),
        );
    }
    options
}

/// Re-roots a relative path the model names into `root`, leaving everything
/// else in the call alone: a `Transform` replaces the arguments object, so the
/// fields the tool needs are copied over rather than rebuilt (FR-POL-2).
struct IntoSandbox {
    root: PathBuf,
}

impl Policy for IntoSandbox {
    fn name(&self) -> &'static str {
        "into-sandbox"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        let Some(path) = path_of(view) else {
            return Decision::Allow;
        };
        if escapes(path) {
            return Decision::Allow;
        }
        let mut arguments = view.call.arguments.clone();
        let Some(object) = arguments.as_object_mut() else {
            return Decision::Allow;
        };
        let landed = self.root.join(path);
        object.insert(
            "path".to_owned(),
            Value::String(landed.to_string_lossy().into_owned()),
        );
        Decision::Transform(arguments)
    }
}

/// Refuses a path that tries to leave the scratch directory.
struct StayInside {
    root: PathBuf,
}

impl Policy for StayInside {
    fn name(&self) -> &'static str {
        "stay-inside"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        match path_of(view) {
            Some(path) if escapes(path) => Decision::Deny(format!(
                "{path} is outside {}; every path is relative and lands in the scratch directory",
                self.root.display()
            )),
            _ => Decision::Allow,
        }
    }
}

/// Narrows the command tool to what the benchmark is for: the crate's own build.
///
/// A command request may name any program, and authorizing it is the host's job,
/// not the executor's. So the host sets the working directory (the request cannot
/// widen it), allows `cargo` and nothing else, and refuses the shell — and it
/// refuses an argument that could reach outside the scratch directory. What comes
/// back, compiler error included, is a tool result the model reads.
struct CargoOnly {
    root: PathBuf,
}

impl Policy for CargoOnly {
    fn name(&self) -> &'static str {
        "cargo-only"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        if view.call.name != "command" {
            return Decision::Allow;
        }
        let mut arguments = view.call.arguments.clone();
        let Some(object) = arguments.as_object_mut() else {
            return Decision::Deny("command: the arguments must be an object".to_owned());
        };
        let command = object.get("command").cloned().unwrap_or(Value::Null);
        if command.get("kind").and_then(Value::as_str) != Some("program") {
            return Decision::Deny(
                "command: the request needs `command` as an object with `kind` and `program`, \
                 for example {\"command\":{\"kind\":\"program\",\"program\":\"cargo\",\
                 \"args\":[\"test\",\"--offline\"]}}; the shell is not available"
                    .to_owned(),
            );
        }
        let Some(program) = command.get("program").and_then(Value::as_str) else {
            return Decision::Deny("command: the program variant needs a `program`".to_owned());
        };
        if program != "cargo" {
            return Decision::Deny(format!(
                "command: only `cargo` runs here, and that request asks for `{program}`"
            ));
        }
        let args = command
            .get("args")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        if let Some(offender) = args
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|arg| arg.starts_with('/') || arg.contains(".."))
        {
            return Decision::Deny(format!(
                "command: `{offender}` could reach outside the scratch directory; \
                 paths are relative to it"
            ));
        }
        object.insert(
            "cwd".to_owned(),
            Value::String(self.root.to_string_lossy().into_owned()),
        );
        Decision::Transform(arguments)
    }
}

/// The `path` argument of every file call, whatever else the call carries.
fn path_of<'b>(view: &PolicyView<'b>) -> Option<&'b str> {
    view.call.arguments.get("path").and_then(Value::as_str)
}

fn escapes(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
}

/// A directory of its own, emptied on every run.
fn scratch() -> Result<PathBuf, Box<dyn Error>> {
    let root = std::env::temp_dir().join("rustychain-bench-agent-cost");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// The wiring: the four executors, the policies that decide their paths and the
/// one program the command tool may run, the model, and the turn options that pin
/// the route.
fn engine(
    root: &Path,
    endpoint: &str,
    model: &str,
) -> Result<(Engine, TurnOptions), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_tool_declaration(ReadTool::new().into_declaration())?;
    tools.register_tool_declaration(WriteTool::new().into_declaration())?;
    tools.register_tool_declaration(EditTool::new().into_declaration())?;
    // The command executor clears the environment and keeps only what the host
    // gives it, so the host gives it its own: `cargo` lives on `PATH`, and a
    // process with no `PATH` cannot find it. The key is in the redaction list, so
    // it cannot surface in a tool result even if a command prints it.
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.remove("CARGO_TARGET_DIR");
    let mut command = CommandTool::new().with_env(env);
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        command = command.with_secrets([key]);
    }
    tools.register_tool_declaration(command.into_declaration())?;

    let policies = PolicySet::new(vec![
        Arc::new(IntoSandbox {
            root: root.to_path_buf(),
        }),
        Arc::new(StayInside {
            root: root.to_path_buf(),
        }),
        Arc::new(CargoOnly {
            root: root.to_path_buf(),
        }),
    ]);

    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build();
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(openrouter(capabilities, endpoint, model)?))
        .tools(tools)
        .policies(policies)
        // A real coding agent reads, edits and reads back over a dozen times; the
        // default budget of 16 is a chat turn's, not a session's, and a request
        // that runs out of budget is one the provider already billed.
        .config(EngineConfig {
            max_iterations: BUDGET,
            ..EngineConfig::default()
        })
        .build()?;
    Ok((engine, turn_options()))
}

/// A variable the benchmark cannot invent a value for: the model and the endpoint
/// belong to the run, not to this file.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(
    capabilities: Capabilities,
    endpoint: &str,
    model: &str,
) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let profile =
        VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, endpoint)?.with_auth(
            AuthScheme::bearer(required("OPENROUTER_API_KEY", "sk-or-...")?),
        );
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(model)?),
        profile,
        capabilities,
    )?)
}
