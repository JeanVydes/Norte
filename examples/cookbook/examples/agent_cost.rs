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
//! before it edits, and the run ends by printing the crate it left behind: the
//! scratch directory is a real, dependency-free crate, so `cd` there and
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
//! The money is the provider's own number, and not one this recipe works out.
//! `Usage` carries the token counters (input, output, reasoning, cache read,
//! cache write, audio), an extension map for the counters with no neutral field,
//! and `vendor`: whatever the payload reported and that vocabulary does not
//! name, kept verbatim. An `OpenAI`-compatible response reports what it billed in
//! `usage.cost`, and the meter sums *that* field per request. No price table
//! lives here on purpose: which upstream answers a request is the router's
//! decision, one request to the next, so a rate written down here would be the
//! list price of a route that may not be the one that served it.
//!
//! What the recipe adds is the tally: how many requests a session took, the
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
//! COOKBOOK_MODEL=deepseek/deepseek-v4-flash-0731 \
//! COOKBOOK_PROVIDER=DeepInfra \
//! cargo run -p rustychain-cookbook --example agent_cost
//! ```
//!
//! `COOKBOOK_PROVIDER` pins the upstream so that two runs stay comparable; it is
//! a routing hint and never a price. Without it the router picks, and the field
//! the response carries says what that route charged.

use std::error::Error;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustychain_core::{
    Capabilities, Decision, Engine, EngineBuilder, EngineConfig, InMemoryBlobStore, InMemoryStore,
    Message, Modality, ModelId, ModelRef, Policy, PolicySet, PolicyView, ProviderId,
    StreamingGranularity, ToolKind, ToolRegistry, TurnOptions, Usage, WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};
use rustychain_tools::{EditTool, ReadTool, WriteTool};
use serde_json::{Value, json};

/// The workflow every task follows: read what is on disk, change it in place,
/// and keep the crate consistent.
const SYSTEM: &str = "\
You are a coding agent working in a scratch directory that already holds the \
`words` crate. Use the tools, never describe what you would do. For each task: \
(1) `read` every file you are about to change, before you change it; (2) `edit` \
the files in place — do not replace a whole file with `write` when the task is a \
change; (3) update every call site, every test and every mention in the README, \
so nothing in the crate still refers to the old shape; (4) answer in one short \
line. Every path is relative to the scratch directory: `src/lib.rs`, \
`tests/words.rs`, `README.md`.";

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
    let root = scratch()?;
    seed(&root)?;
    let (engine, options) = engine(&root)?;
    let provider = std::env::var("COOKBOOK_PROVIDER")
        .unwrap_or_else(|_| "whoever the router picks".to_owned());

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
        "money:    the `usage.cost` every response reports, summed; the recipe applies no price \
         of its own\n"
    );

    let mut ledger = Ledger::default();
    for (index, task) in TASKS.iter().enumerate() {
        println!("--- task {}: {task}", index + 1);
        let session = engine.create().await?;
        engine
            .append_history(session.id, vec![Message::system(SYSTEM)])
            .await?;
        // A turn that runs out of budget is an `Err`, but the requests it made
        // are already committed and already billed: the tally runs either way,
        // and only the last line says whether the agent finished.
        let outcome = engine
            .turn(session.id)
            .input(Message::user(*task))
            .options(options.clone())
            .start()
            .finish()
            .await;

        let mut task = Task::default();
        for request in engine.history(session.id).await? {
            let Some(usage) = request.origin.as_ref().map(|origin| &origin.usage) else {
                continue;
            };
            let line = task.count(usage);
            println!("  request {}: {line}", task.requests);
        }
        match outcome {
            Ok(outcome) => println!(
                "  answer:   {}",
                outcome
                    .message
                    .as_ref()
                    .map(Message::text)
                    .unwrap_or_default()
            ),
            Err(error) => {
                println!("  failed:   {error} — the requests above are what it cost to find out");
            }
        }
        println!("  task:     {}\n", task.line());
        ledger.add(&task);
    }

    closing(&ledger);

    // The evidence, not the model's word: the crate as the agent left it.
    println!("\n--- the crate as the agent left it");
    print_tree(&root);
    Ok(())
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

/// One task's tally: the counters the provider reported, and the money it
/// reported for them.
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
}

impl Task {
    /// The price the payload itself carries, if it carries one.
    ///
    /// `cost` is `OpenAI`-compatible, and it reaches here untouched: it survives
    /// the parse in `Usage::vendor`, because the vocabulary does not name it. The
    /// recipe reads that field; it never writes a price of its own.
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
             wire reports usage without a price, and a recipe cannot invent one"
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
    let root = std::env::temp_dir().join("rustychain-cookbook-agent-cost");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// The wiring: the three file executors, the policy that decides their paths,
/// the model, and the turn options that pin the route.
fn engine(root: &Path) -> Result<(Engine, TurnOptions), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_tool_declaration(ReadTool::new().into_declaration())?;
    tools.register_tool_declaration(WriteTool::new().into_declaration())?;
    tools.register_tool_declaration(EditTool::new().into_declaration())?;

    let policies = PolicySet::new(vec![
        Arc::new(IntoSandbox {
            root: root.to_path_buf(),
        }),
        Arc::new(StayInside {
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
        .model(Arc::new(openrouter(capabilities)?))
        .tools(tools)
        .policies(policies)
        // A real coding agent reads, edits and reads back over a dozen times; the
        // default budget of 16 is a chat turn's, not a session's, and a request
        // that runs out of budget is one the provider already billed.
        .config(EngineConfig {
            max_iterations: 32,
            ..EngineConfig::default()
        })
        .build()?;
    Ok((engine, turn_options()))
}

/// The model every recipe runs on unless `COOKBOOK_MODEL` says otherwise.
const DEFAULT_MODEL: &str = "nvidia/nemotron-3.5-super-vl:free";
/// `OpenRouter`, spoken over the `OpenAI`-compatible Chat Completions wire.
const ENDPOINT: &str = "https://openrouter.ai/api/v1";

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, ENDPOINT)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = std::env::var("COOKBOOK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?)
}
