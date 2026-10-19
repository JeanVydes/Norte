//! The host's half of the effect boundary: make a tool's external effect happen
//! once, even when the engine delivers the call again after a crash.
//!
//! Norte guarantees **commit-at-most-once**: a turn is never lost, and a tool
//! result that was already committed is never produced twice (FR-ERR-5,
//! FR-APPR-8). It does not promise **effect-at-most-once** (FR-ERR-6): the
//! window between a tool's external effect — a charge, an email, a write to
//! another system — and its commit is outside any library, so a process killed
//! there is handed the same call again on `resume`.
//!
//! This pattern closes that window in host code, and needs nothing from the
//! engine beyond what a tool already receives: the session and the
//! engine-assigned `call_id` (FR-ID-2), both on `ToolContext`. It is a durable
//! `intent -> effect -> done` record keyed by the call, plus an idempotency key
//! handed to the upstream, so the one genuinely ambiguous case — the effect
//! happened but `done` did not commit — is resolved by asking, not by guessing.
//!
//! ```sh
//! cargo run -p norte-cookbook --example idempotent_tool
//! ```
//!
//! Offline: no model and no key. The upstream is a stand-in that behaves like
//! one that accepts an idempotency key: same key, one effect, and a lookup by
//! key for the ambiguous case.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Mutex;

use norte_core::{
    CallId, Error as NorteError, FnExecutor, Result as NorteResult, SessionId, ToolContext,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let session = SessionId::new();

    println!("── 1. delivered once ────────────────────────────────");
    let tool = ChargeTool::new();
    let context = call_context(session, CallId::new());
    let key = effect_key(&context).expect("the call carries an identity");
    let charge = tool.call_with_context(&context, args(500)).await?;
    report("first delivery", &charge, tool.upstream.effects());

    println!("\n── 2. the engine delivers the same call again ───────");
    println!("the tool message never committed, so the call comes back");
    let again = tool.call_with_context(&context, args(500)).await?;
    report("redelivery", &again, tool.upstream.effects());
    assert_eq!(again, charge, "the same charge, not a second one");

    println!("\n── 3. crashed between the effect and `done` ─────────");
    println!("the record says intended; the charge did reach upstream");
    let tool = ChargeTool::new();
    tool.ledger.intend(&key);
    tool.upstream.charge(&key, 500);
    let recovered = tool.call_with_context(&context, args(500)).await?;
    report(
        "resolved by asking upstream",
        &recovered,
        tool.upstream.effects(),
    );
    assert_eq!(recovered, charge, "the existing charge is adopted");

    println!("\n── 4. crashed before the effect ─────────────────────");
    println!("the record says intended; upstream has nothing");
    let tool = ChargeTool::new();
    tool.ledger.intend(&key);
    let ran = tool.call_with_context(&context, args(500)).await?;
    report("re-run, exactly once", &ran, tool.upstream.effects());
    assert_eq!(ran, charge, "the effect finally happens, once");

    println!("\nthe effect happened once, whatever the crash did.");
    Ok(())
}

/// Prints one delivery: what the tool returned and how many effects the
/// upstream actually applied.
fn report(label: &str, charge: &Charge, effects: u64) {
    println!("{label:<28} {} for {} cents", charge.id, charge.cents);
    println!("{:<28} upstream effects = {effects}", "");
}

/// The tool is called by the engine with the call's own identity on the
/// context; a direct caller has none, which is a typed error rather than a
/// silently unkeyed effect.
fn effect_key(context: &ToolContext) -> NorteResult<String> {
    match (context.session, context.call_id) {
        (Some(session), Some(call)) => Ok(format!("{session}:{call}")),
        _ => Err(NorteError::Other(
            "the charge tool needs the session and call id the engine provides".to_owned(),
        )),
    }
}

fn call_context(session: SessionId, call: CallId) -> ToolContext {
    ToolContext::new().with_session(session).with_call(call)
}

fn args(cents: u64) -> ChargeArgs {
    ChargeArgs {
        customer: "cus_demo".to_owned(),
        cents,
    }
}

// ── the tool ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChargeArgs {
    customer: String,
    cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Charge {
    id: String,
    cents: u64,
}

/// A tool with an external effect: it charges a customer.
///
/// It is idempotent not by magic but by construction: a durable ledger of what
/// it intends and what it has done, and an effect key the upstream understands.
#[derive(Default)]
struct ChargeTool {
    ledger: Ledger,
    upstream: Upstream,
}

impl ChargeTool {
    fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl FnExecutor<ChargeArgs, Charge> for ChargeTool {
    /// Direct callers have no call identity, so the effect cannot be keyed.
    /// The engine always uses [`FnExecutor::call_with_context`].
    async fn call(&self, args: ChargeArgs) -> NorteResult<Charge> {
        self.call_with_context(&ToolContext::new(), args).await
    }

    async fn call_with_context(
        &self,
        context: &ToolContext,
        args: ChargeArgs,
    ) -> NorteResult<Charge> {
        let key = effect_key(context)?;
        match self.ledger.slot(&key) {
            // Already done by a previous delivery: the effect must not repeat.
            Some(Slot::Done(charge)) => return Ok(charge),
            // Intended but not done: the crash window. Ask the upstream, which
            // deduplicates on the same key, whether the effect happened.
            Some(Slot::Intended) => {
                if let Some(charge) = self.upstream.find(&key) {
                    self.ledger.done(&key, charge.clone());
                    return Ok(charge);
                }
            }
            None => {}
        }
        self.ledger.intend(&key);
        let charge = self.upstream.charge(&key, args.cents);
        self.ledger.done(&key, charge.clone());
        Ok(charge)
    }
}

// ── the host's durable record ───────────────────────────────────────────────

/// What the host has written down for one effect key.
#[derive(Clone)]
enum Slot {
    /// The effect is about to happen; it may or may not have happened.
    Intended,
    /// The effect happened and its result is durable.
    Done(Charge),
}

/// The host's store. A real host writes this to its own database in the same
/// transaction as the business row; here it is a map.
#[derive(Default)]
struct Ledger {
    rows: Mutex<BTreeMap<String, Slot>>,
}

impl Ledger {
    fn slot(&self, key: &str) -> Option<Slot> {
        self.rows
            .lock()
            .expect("the ledger lock is never held across an await")
            .get(key)
            .cloned()
    }

    fn intend(&self, key: &str) {
        self.rows
            .lock()
            .expect("the ledger lock is never held across an await")
            .entry(key.to_owned())
            .or_insert(Slot::Intended);
    }

    fn done(&self, key: &str, charge: Charge) {
        self.rows
            .lock()
            .expect("the ledger lock is never held across an await")
            .insert(key.to_owned(), Slot::Done(charge));
    }
}

// ── the upstream ────────────────────────────────────────────────────────────

/// A stand-in for a provider that accepts an idempotency key: the same key
/// applies one effect, and the effect can be looked up by that key.
#[derive(Default)]
struct Upstream {
    by_key: Mutex<BTreeMap<String, Charge>>,
    effects: Mutex<u64>,
}

impl Upstream {
    fn charge(&self, key: &str, cents: u64) -> Charge {
        let mut by_key = self
            .by_key
            .lock()
            .expect("the upstream lock is never held across an await");
        if let Some(existing) = by_key.get(key) {
            return existing.clone();
        }
        *self
            .effects
            .lock()
            .expect("the upstream lock is never held across an await") += 1;
        let charge = Charge {
            id: format!("ch_{}", by_key.len() + 1),
            cents,
        };
        by_key.insert(key.to_owned(), charge.clone());
        charge
    }

    fn find(&self, key: &str) -> Option<Charge> {
        self.by_key
            .lock()
            .expect("the upstream lock is never held across an await")
            .get(key)
            .cloned()
    }

    fn effects(&self) -> u64 {
        *self
            .effects
            .lock()
            .expect("the upstream lock is never held across an await")
    }
}
