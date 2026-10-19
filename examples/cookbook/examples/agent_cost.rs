//! What an agentic session costs: the loop of `agent`, with the meter on.
//!
//! Three small coding tasks go through the same engine, each in a session of its
//! own, and **every model request the engine made is printed** — including the
//! ones inside a tool loop, which a turn's outcome alone would hide: the outcome
//! carries the last answer, while each iteration committed an assistant message
//! of its own, with the usage that produced it.
//!
//! The money is computed here, not read from the response. `Usage` carries token
//! counters (input, output, reasoning, cache read, cache write, audio), an
//! extension map for the counters with no neutral field, and `vendor`: whatever
//! the payload reported and this vocabulary does not name, kept verbatim. An
//! `OpenAI`-compatible response carries its own `usage.cost` in there, so the
//! price table below is the arithmetic and the vendor's field is printed beside
//! it — a price table is a list price, and the payload is what was billed.
//!
//! Three things about that arithmetic are easy to get wrong:
//!
//! 1. **Cached tokens are inside `input`.** An `OpenAI`-compatible provider
//!    counts what it read from the cache as part of `prompt_tokens`, so the
//!    billed input is `input - cache_read` and the hit rate is
//!    `cache_read / input`; counting the whole input at the full rate
//!    double-bills the hit. The recipe prints the counterfactual — the same
//!    tokens with no cache at all — next to the bill.
//! 2. **The published price is not always the billed price.** The table below is
//!    a *list* price, and the closing shows the vendor's own field next to what
//!    the table says, so the gap is visible instead of assumed.
//!    `COOKBOOK_PRICE_IN`, `COOKBOOK_PRICE_OUT` and `COOKBOOK_PRICE_CACHE_READ`
//!    override the table when a run has to be priced with a rate it does not
//!    carry.
//! 3. **A hit rate is not a bill reduction.** A cached token saves only the gap
//!    between the input rate and the read rate, and the output side pays its own
//!    rate either way, so the whole-bill share is below the input-side share —
//!    slightly in an agentic session, where the input dominates (this run is
//!    roughly 37 input tokens per output token), and heavily in a chatty one.
//!    Both are printed, and neither is the raw hit rate.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_MODEL=deepseek/deepseek-v4-flash-0731 \
//! COOKBOOK_PROVIDER=Relace \
//! cargo run -p rustychain-cookbook --example agent_cost
//! ```

use std::error::Error;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustychain_core::{
    Capabilities, Decision, Engine, EngineBuilder, InMemoryBlobStore, InMemoryStore, Message,
    Modality, ModelId, ModelRef, Policy, PolicySet, PolicyView, ProviderId, StreamingGranularity,
    ToolKind, ToolRegistry, TurnOptions, Usage, WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};
use rustychain_tools::{EditTool, ReadTool, WriteTool};
use serde_json::{Value, json};

/// The workflow every task follows, so the loop has something to iterate on.
const SYSTEM: &str = "\
You are a coding agent in a scratch directory. Use the tools, never describe what \
you would do. For each task: (1) `write` the file the task names; (2) `read` it \
back to confirm what is on disk; (3) if the task asks for a change, `edit` the \
file; (4) answer in one short line. Every path is relative and lands in the \
scratch directory.";

/// Three sessions of the same shape: create, look, change, report.
const TASKS: [&str; 3] = [
    "Write `hello.rs` with a `main` that prints `hello` and nothing else. Read it back. Answer with the one line it prints.",
    "Read `hello.rs`, change the printed text to `hello, harness`, and leave everything else identical. Answer with the final contents of the file.",
    "Write `notes.md` with exactly two lines — `what: hello.rs` and `changed: yes` — then read it back and answer with its contents.",
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = scratch()?;
    let (engine, options) = engine(&root)?;
    let (provider, price) = prices();

    println!("scratch:  {}", root.display());
    println!("provider: {provider}");
    match price {
        Some(price) => println!(
            "prices:   ${:.4}/M input · ${:.4}/M output · ${:.5}/M cache read\n",
            price.input, price.output, price.cache_read
        ),
        None => println!(
            "prices:   none published for a route this recipe does not know, so tokens are \
             counted and money is not\n"
        ),
    }

    let mut ledger = Ledger::default();
    for (index, task) in TASKS.iter().enumerate() {
        println!("--- task {}: {task}", index + 1);
        let session = engine.create().await?;
        engine
            .append_history(session.id, vec![Message::system(SYSTEM)])
            .await?;
        let outcome = engine
            .turn(session.id)
            .input(Message::user(*task))
            .options(options.clone())
            .start()
            .finish()
            .await?;

        let mut task = Task::default();
        for request in engine.history(session.id).await? {
            let Some(usage) = request.origin.as_ref().map(|origin| &origin.usage) else {
                continue;
            };
            let line = task.count(usage, price.as_ref());
            println!("  request {}: {line}", task.requests);
        }
        println!(
            "  answer:   {}",
            outcome
                .message
                .as_ref()
                .map(Message::text)
                .unwrap_or_default()
        );
        println!("  task:     {}\n", task.line(price.as_ref()));
        ledger.add(&task);
    }

    closing(&ledger, price.as_ref());
    Ok(())
}

/// One request's usage, as the provider reported it, inside the task's tally.
#[derive(Default)]
struct Task {
    requests: u64,
    input: u64,
    cached: u64,
    output: u64,
    cost: f64,
    uncached: f64,
    /// The vendor's own `usage.cost`, when a provider reported one. It is the
    /// only exact price in the exchange and it survives the parse in `vendor`.
    reported: Option<f64>,
}

impl Task {
    /// The price the payload itself carries, if it carries one.
    fn reported_cost(usage: &Usage) -> Option<f64> {
        usage.vendor.get("cost").and_then(Value::as_f64)
    }

    /// Folds one request in and returns the line for it alone.
    fn count(&mut self, usage: &Usage, price: Option<&Price>) -> String {
        let input = usage.input_tokens.unwrap_or(0);
        let cached = usage.cache_read_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        self.requests += 1;
        self.input += input;
        self.cached += cached;
        self.output += output;
        let reported = Self::reported_cost(usage);
        if let Some(reported) = reported {
            *self.reported.get_or_insert(0.0) += reported;
        }
        let reported = reported
            .map(|reported| format!(" · vendor ${reported:.6}"))
            .unwrap_or_default();
        let line = format!(
            "input {input} · output {output} · cache read {cached}{}{reported}",
            hit_of(cached, input)
        );
        let Some(price) = price else {
            return line;
        };
        let cost = price.billed(usage);
        self.cost += cost;
        self.uncached += price.uncached(usage);
        format!("{line} · ${cost:.6}")
    }

    fn line(&self, price: Option<&Price>) -> String {
        let tokens = format!(
            "{} request(s) · input {} · output {} · cache read {}{}",
            self.requests,
            self.input,
            self.output,
            self.cached,
            hit_of(self.cached, self.input)
        );
        let reported = self
            .reported
            .map(|reported| format!(" · vendor ${reported:.6}"))
            .unwrap_or_default();
        match price {
            Some(_) => format!(
                "{tokens} · ${:.6} (${:.6} with no cache at all){reported}",
                self.cost, self.uncached
            ),
            None => format!("{tokens}{reported}"),
        }
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
    cost: f64,
    uncached: f64,
    reported: Option<f64>,
}

impl Ledger {
    fn add(&mut self, task: &Task) {
        self.tasks += 1;
        self.requests += task.requests;
        self.input += task.input;
        self.cached += task.cached;
        self.output += task.output;
        self.cost += task.cost;
        self.uncached += task.uncached;
        if let Some(reported) = task.reported {
            *self.reported.get_or_insert(0.0) += reported;
        }
    }
}

/// The closing word: what one task costs, and what a day of them would.
fn closing(ledger: &Ledger, price: Option<&Price>) {
    println!("--- what this shows");
    println!(
        "{} task(s), {} model request(s): input {} · output {} · cache read {}",
        ledger.tasks, ledger.requests, ledger.input, ledger.output, ledger.cached
    );
    let tasks = as_f64(ledger.tasks.max(1));
    if let Some(reported) = ledger.reported {
        println!(
            "the provider's own `usage.cost`, summed over every request: ${reported:.6} \
             (${:.6} per task)",
            reported / tasks
        );
    }
    if let Some(hit) = hit_rate(ledger.cached, ledger.input) {
        println!(
            "cache hit: {hit:.1} % of the input ({} of {} tokens were read from the provider's \
             cache)",
            ledger.cached, ledger.input
        );
    }
    let Some(price) = price else {
        println!(
            "no price was applied, so the tokens above and the provider's own field are the \
             whole statement. Name the provider with COOKBOOK_PROVIDER to price the run."
        );
        return;
    };
    let per_task = ledger.cost / tasks;
    println!(
        "cost: ${:.6} for the session, ${:.6} per task, ${:.4} for a hundred tasks",
        ledger.cost,
        per_task,
        per_task * 100.0
    );
    let saved = ledger.uncached - ledger.cost;
    // Why the whole-bill share is not the hit rate: a cached token saves the
    // difference between the input rate and the read rate, and the output side
    // pays its own rate either way, so it dilutes the share.
    let input_side = 100.0 * as_f64(ledger.cached) * (price.input - price.cache_read)
        / (as_f64(ledger.input) * price.input).max(f64::MIN_POSITIVE);
    println!(
        "the same tokens with no cache at all would have been ${:.6}: the cache saved ${:.6} — \
         {input_side:.1} % of the input side (read at ${:.5}/M against ${:.4}/M input), {:.1} % \
         of the whole bill, because output tokens get no cache rate",
        ledger.uncached,
        saved,
        price.cache_read,
        price.input,
        100.0 * saved / ledger.uncached.max(f64::MIN_POSITIVE)
    );
    if let Some(reported) = ledger.reported {
        if reported > 0.0 && (reported - ledger.cost).abs() > 1e-9 {
            println!(
                "the table is ${:.6} away from what the provider billed: a list price is not \
                 always the billed price, and the payload's own field is the one that settles it",
                reported - ledger.cost
            );
        }
    }
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

/// USD per million tokens, as the provider publishes them.
#[derive(Clone, Copy)]
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
}

impl Price {
    /// What the request cost. The provider counts the tokens it read from the
    /// cache inside `input`, so only the uncached ones pay the input rate.
    fn billed(&self, usage: &Usage) -> f64 {
        let input = usage.input_tokens.unwrap_or(0);
        let cached = usage.cache_read_tokens.unwrap_or(0).min(input);
        let fresh = input - cached;
        let output = usage.output_tokens.unwrap_or(0);
        (as_f64(fresh) * self.input
            + as_f64(cached) * self.cache_read
            + as_f64(output) * self.output)
            / 1_000_000.0
    }

    /// The same request with every input token at the input rate: the
    /// counterfactual a cache is supposed to beat.
    fn uncached(&self, usage: &Usage) -> f64 {
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        (as_f64(input) * self.input + as_f64(output) * self.output) / 1_000_000.0
    }
}

/// The published price of the route, if the recipe knows it.
fn prices() -> (String, Option<Price>) {
    let provider =
        std::env::var("COOKBOOK_PROVIDER").unwrap_or_else(|_| "OpenRouter decides".to_owned());
    let published = match provider.as_str() {
        "Relace" => Some(Price {
            input: 0.0114,
            output: 0.45,
            cache_read: 0.0114,
        }),
        "OpenInference" => Some(Price {
            input: 0.01208,
            output: 0.12,
            cache_read: 0.00224,
        }),
        "DeepInfra" => Some(Price {
            input: 0.06,
            output: 0.18,
            cache_read: 0.015,
        }),
        _ => None,
    };
    let Some(table) = published else {
        return (provider, None);
    };
    let over = |name: &str, fallback: f64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(fallback)
    };
    (
        provider,
        Some(Price {
            input: over("COOKBOOK_PRICE_IN", table.input),
            output: over("COOKBOOK_PRICE_OUT", table.output),
            cache_read: over("COOKBOOK_PRICE_CACHE_READ", table.cache_read),
        }),
    )
}

/// Pins the upstream provider when `COOKBOOK_PROVIDER` names one.
///
/// A harness that measures money has to know who it paid: with `OpenRouter` free
/// to route, "the model" is a different endpoint from one request to the next,
/// with different prices.
fn turn_options() -> TurnOptions {
    let mut options = TurnOptions::default();
    options.params.max_output_tokens = Some(512);
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
