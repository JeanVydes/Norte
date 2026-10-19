//! What a `429`, a `5xx` and a dead connection cost under a host retry policy,
//! with the library deliberately doing nothing about them.
//!
//! The contract this measures is a deliberate split (FR-ERR-4, FR-ERR-5). The
//! transport does **not** retry: a provider's answer reaches the host as a typed
//! [`ProviderError`] with its kind, its status and `Retry-After` intact, and
//! `is_transient` to say whether waiting could clear it. Every decision after
//! that — wait or not, how long, how many times — is the host's, because only
//! the host knows whether the request is idempotent and what it is willing to
//! pay.
//!
//! So the benchmark wraps a real [`Engine`] in a host policy and watches three
//! things for each scenario:
//!
//! 1. **What was surfaced** — the kind, status, `Retry-After` and the
//!    transient verdict of every failure, not a flattened error string.
//! 2. **What the host paid** — attempts, and the wall-clock wait the policy
//!    chose. A run here sleeps a millisecond per wait instead of the provider's
//!    ask, so an offline run stays a benchmark and not a nap; the modelled wait
//!    is reported beside it.
//! 3. **That a retry is the same request** — the resumed turn re-sends a
//!    request whose bytes are `sha256`-identical to the first, so the provider's
//!    cache still applies and the retry is not a new prefix.
//!
//! The model is scripted, so every scenario is deterministic and runs in CI with
//! no key and no network.
//!
//! ```sh
//! cargo run -p norte-bench-transport-retry -- --rounds 20
//! ```

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run};
use norte_core::{
    Capabilities, ContinuityAdapter, Engine, EngineBuilder, Error as NorteError, FinishReason,
    InMemoryBlobStore, InMemoryStore, Message, Model, ModelId, ModelRef, ModelRequest, ModelStream,
    ProviderError, ProviderErrorKind, ProviderId, SessionId, StreamEvent,
};
use norte_http::is_transient;
use norte_testing::{OpaqueContinuity, text_capabilities};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

const USAGE: &str = "\
norte-bench-transport-retry — 429/5xx/timeout under a host retry policy

USAGE:
    norte-bench-transport-retry [OPTIONS]

OPTIONS:
    --rounds <n>   repeat every scenario n times, for timing (default 20)
    --report <p>   write the markdown report to <p>
    --jsonl <p>    write the JSONL trail to <p>
    --help, -h     print this help

Offline: no provider is called. The failures are scripted and deterministic;
the host policy is the one a real caller would write. The library never
retries; every attempt below is the host deciding to make one.
";

/// How long the benchmark actually sleeps during a described wait.
const SIMULATED_SLEEP: Duration = Duration::from_millis(1);

/// The host's retry policy: how many attempts, and the longest wait it will
/// accept from a provider that asks for more.
struct Policy {
    max_attempts: u32,
    ceiling: Duration,
}

/// One injected provider failure.
struct Failure {
    kind: ProviderErrorKind,
    status: Option<u16>,
    retry_after: Option<Duration>,
}

impl Failure {
    fn to_error(&self) -> NorteError {
        let mut error = ProviderError::new(self.kind, "scripted failure");
        if let Some(status) = self.status {
            error = error.with_status(status);
        }
        if let Some(retry_after) = self.retry_after {
            error = error.with_retry_after(retry_after);
        }
        error.into()
    }

    fn describe(&self) -> String {
        let mut text = format!("{:?}", self.kind);
        if let Some(status) = self.status {
            let _ = write!(text, " {status}");
        }
        if let Some(retry_after) = self.retry_after {
            let _ = write!(text, " retry-after {}", millis(retry_after));
        }
        text
    }
}

/// A model that fails the first scripted turns and answers the rest.
struct FlakyModel {
    reference: ModelRef,
    capabilities: Capabilities,
    continuity: OpaqueContinuity,
    plan: Vec<Failure>,
    calls: AtomicU64,
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl FlakyModel {
    fn new(plan: Vec<Failure>) -> Self {
        Self {
            reference: ModelRef::new(
                ProviderId::new("offline").expect("provider id"),
                ModelId::new("scripted").expect("model id"),
            ),
            capabilities: text_capabilities(),
            continuity: OpaqueContinuity,
            plan,
            calls: AtomicU64::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// The `sha256` of every reconstructed request, in order.
    fn digests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log is never held across an await")
            .iter()
            .map(|request| {
                let mut hasher = Sha256::new();
                hasher.update(
                    serde_json::to_string(request)
                        .unwrap_or_default()
                        .as_bytes(),
                );
                format!("{:x}", hasher.finalize())
            })
            .collect()
    }
}

#[async_trait]
impl Model for FlakyModel {
    fn reference(&self) -> &ModelRef {
        &self.reference
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        &self.continuity
    }

    async fn stream(&self, request: ModelRequest) -> norte_core::Result<ModelStream> {
        self.requests
            .lock()
            .expect("the request log is never held across an await")
            .push(request);
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.plan.get(usize::try_from(index).unwrap_or(usize::MAX)) {
            Some(failure) => Err(failure.to_error()),
            None => Ok(ok_stream()),
        }
    }
}

fn ok_stream() -> ModelStream {
    let events = vec![
        StreamEvent::TextDelta {
            index: 0,
            text: "ok".to_owned(),
        },
        StreamEvent::Finish(FinishReason::Stop),
    ];
    Box::pin(futures_util::stream::iter(
        events.into_iter().map(Ok::<StreamEvent, NorteError>),
    ))
}

/// One entry in a policy's trace.
#[derive(Serialize, Clone)]
struct Attempt {
    #[serde(rename = "attempt")]
    index: u32,
    ok: bool,
    /// Whether the host decided to make another attempt after this one.
    retried: bool,
    kind: Option<String>,
    status: Option<u16>,
    retry_after_ms: Option<u64>,
    transient: bool,
    waited_ms: u64,
}

/// One run of a scenario, before it is folded into the summary.
struct Round {
    trace: Vec<Attempt>,
    ok: bool,
    attempts: u32,
    modelled_wait_ms: u64,
    requests: u64,
    stable_requests: bool,
    error: Option<String>,
}

/// The summary of one scenario across its rounds.
#[derive(Serialize)]
struct ScenarioRecord {
    scenario: String,
    plan: Vec<String>,
    max_attempts: u32,
    rounds: u64,
    ok: bool,
    attempts: u32,
    modelled_wait_ms: u64,
    requests: u64,
    stable_requests: bool,
    min_micros: u64,
    median_micros: u64,
    max_micros: u64,
    trace: Vec<Attempt>,
    error: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "control",
            plan: vec![],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "one-429-with-retry-after",
            plan: vec![Failure {
                kind: ProviderErrorKind::RateLimited,
                status: Some(429),
                retry_after: Some(Duration::from_secs(2)),
            }],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "two-429s-second-silent",
            plan: vec![
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: Some(Duration::from_millis(300)),
                },
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: None,
                },
            ],
            policy: Policy {
                max_attempts: 4,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "three-500s",
            plan: vec![
                Failure {
                    kind: ProviderErrorKind::ServerError,
                    status: Some(500),
                    retry_after: None,
                },
                Failure {
                    kind: ProviderErrorKind::ServerError,
                    status: Some(502),
                    retry_after: None,
                },
                Failure {
                    kind: ProviderErrorKind::ServerError,
                    status: Some(503),
                    retry_after: None,
                },
            ],
            policy: Policy {
                max_attempts: 5,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "overloaded-then-ok",
            plan: vec![Failure {
                kind: ProviderErrorKind::ModelOverloaded,
                status: Some(503),
                retry_after: None,
            }],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "transport-timeout",
            plan: vec![Failure {
                kind: ProviderErrorKind::Transport,
                status: None,
                retry_after: None,
            }],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "exhausts-max-attempts",
            plan: vec![
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: None,
                },
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: None,
                },
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: None,
                },
                Failure {
                    kind: ProviderErrorKind::RateLimited,
                    status: Some(429),
                    retry_after: None,
                },
            ],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
        Scenario {
            name: "permanent-401",
            plan: vec![Failure {
                kind: ProviderErrorKind::Authentication,
                status: Some(401),
                retry_after: None,
            }],
            policy: Policy {
                max_attempts: 3,
                ceiling: Duration::from_secs(5),
            },
        },
    ]
}

struct Scenario {
    name: &'static str,
    plan: Vec<Failure>,
    policy: Policy,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "transport-retry",
        USAGE,
        Mode::Offline,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let rounds = run.cli().option_u64("rounds")?.unwrap_or(20).max(1);
    let mut records = Vec::new();
    for scenario in scenarios() {
        let record = run_scenario(&scenario, rounds).await;
        run.record("scenario", &record);
        records.push(record);
    }

    let report = render(&records, rounds);
    run.table("scenarios", &SUMMARY_HEADERS, summary_rows(&records));
    run.table("attempts", &ATTEMPT_HEADERS, attempt_rows(&records));
    run.finish(report)?;
    Ok(())
}

async fn run_scenario(scenario: &Scenario, rounds: u64) -> ScenarioRecord {
    let mut micros = Vec::new();
    let mut last: Option<Round> = None;
    for _ in 0..rounds {
        let started = Instant::now();
        let round = one_round(scenario).await;
        micros.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        last = Some(round);
    }
    let mut sorted = micros.clone();
    sorted.sort_unstable();
    let round = last.expect("at least one round");
    ScenarioRecord {
        scenario: scenario.name.to_owned(),
        plan: scenario
            .plan
            .iter()
            .map(Failure::describe)
            .collect::<Vec<_>>(),
        max_attempts: scenario.policy.max_attempts,
        rounds,
        ok: round.ok,
        attempts: round.attempts,
        modelled_wait_ms: round.modelled_wait_ms,
        requests: round.requests,
        stable_requests: round.stable_requests,
        min_micros: sorted.first().copied().unwrap_or(0),
        median_micros: sorted.get(sorted.len() / 2).copied().unwrap_or(0),
        max_micros: sorted.last().copied().unwrap_or(0),
        trace: round.trace,
        error: round.error,
    }
}

/// Runs one round against a fresh engine.
async fn one_round(scenario: &Scenario) -> Round {
    let model = Arc::new(FlakyModel::new(
        scenario
            .plan
            .iter()
            .map(|failure| Failure {
                kind: failure.kind,
                status: failure.status,
                retry_after: failure.retry_after,
            })
            .collect(),
    ));
    let engine = engine(Arc::clone(&model));
    let session = engine
        .create()
        .await
        .expect("the offline store always creates a session");
    let (trace, error) = run_with_policy(
        &engine,
        session.id,
        Message::user("Reply with the single word: ok."),
        &scenario.policy,
    )
    .await;
    let attempts = u32::try_from(trace.len()).unwrap_or(u32::MAX);
    let modelled_wait_ms = trace.iter().map(|attempt| attempt.waited_ms).sum();
    let digests = model.digests();
    let stable_requests = digests.windows(2).all(|pair| pair[0] == pair[1]);
    Round {
        trace,
        ok: error.is_none(),
        attempts,
        modelled_wait_ms,
        requests: model.calls(),
        stable_requests,
        error,
    }
}

/// The host policy, exactly as a caller would write it: classify, wait as the
/// provider asked (or back off), resume the same session, and stop at the cap.
async fn run_with_policy(
    engine: &Engine,
    session: SessionId,
    message: Message,
    policy: &Policy,
) -> (Vec<Attempt>, Option<String>) {
    let mut stream = engine.send(session, message);
    let mut attempt = 1_u32;
    let mut trace = Vec::new();
    loop {
        match stream.finish().await {
            Ok(_) => {
                trace.push(Attempt {
                    index: attempt,
                    ok: true,
                    retried: false,
                    kind: None,
                    status: None,
                    retry_after_ms: None,
                    transient: false,
                    waited_ms: 0,
                });
                return (trace, None);
            }
            Err(error) => {
                let kind = error.provider_kind();
                let transient = is_transient(&error);
                let retry_after = error.retry_after();
                let status = match &error {
                    NorteError::Provider(provider) => provider.status,
                    _ => None,
                };
                if transient && attempt < policy.max_attempts {
                    let wait = retry_after
                        .unwrap_or_else(|| backoff(attempt))
                        .min(policy.ceiling);
                    trace.push(Attempt {
                        index: attempt,
                        ok: false,
                        retried: true,
                        kind: kind.map(|kind| format!("{kind:?}")),
                        status,
                        retry_after_ms: retry_after.map(millis),
                        transient,
                        waited_ms: millis(wait),
                    });
                    tokio::time::sleep(wait.min(SIMULATED_SLEEP)).await;
                    attempt += 1;
                    stream = engine.resume(session);
                } else {
                    trace.push(Attempt {
                        index: attempt,
                        ok: false,
                        retried: false,
                        kind: kind.map(|kind| format!("{kind:?}")),
                        status,
                        retry_after_ms: retry_after.map(millis),
                        transient,
                        waited_ms: 0,
                    });
                    return (trace, Some(format!("{kind:?}: {error}")));
                }
            }
        }
    }
}

fn engine(model: Arc<FlakyModel>) -> Engine {
    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(model as Arc<dyn Model>)
        .build()
        .expect("the offline engine always builds")
}

/// The host's own choice when the provider did not say how long to wait:
/// exponential, capped, so a burst cannot turn into a hot loop.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2_u64.pow(attempt.min(4))).min(Duration::from_secs(5))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

const SUMMARY_HEADERS: [&str; 10] = [
    "scenario",
    "plan",
    "max_attempts",
    "attempts",
    "ok",
    "modelled_wait_ms",
    "requests",
    "stable_requests",
    "median_micros",
    "error",
];

fn summary_rows(records: &[ScenarioRecord]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|record| {
            vec![
                record.scenario.clone(),
                record.plan.join(" | "),
                record.max_attempts.to_string(),
                record.attempts.to_string(),
                record.ok.to_string(),
                record.modelled_wait_ms.to_string(),
                record.requests.to_string(),
                record.stable_requests.to_string(),
                record.median_micros.to_string(),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect()
}

const ATTEMPT_HEADERS: [&str; 9] = [
    "scenario",
    "attempt",
    "ok",
    "retried",
    "kind",
    "status",
    "retry_after_ms",
    "transient",
    "waited_ms",
];

fn attempt_rows(records: &[ScenarioRecord]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for record in records {
        for attempt in &record.trace {
            rows.push(vec![
                record.scenario.clone(),
                attempt.index.to_string(),
                attempt.ok.to_string(),
                attempt.retried.to_string(),
                attempt.kind.clone().unwrap_or_default(),
                attempt.status.map_or_else(String::new, |s| s.to_string()),
                attempt
                    .retry_after_ms
                    .map_or_else(String::new, |v| v.to_string()),
                attempt.transient.to_string(),
                attempt.waited_ms.to_string(),
            ]);
        }
    }
    rows
}

#[allow(clippy::too_many_lines)]
fn render(records: &[ScenarioRecord], rounds: u64) -> Report {
    let mut report = Report::new("Transport retry under host policy");
    report.paragraph(
        "The transport never retries (FR-ERR-4): a `429`, a `5xx` or a dead connection \
         reaches the host as a typed error, `Retry-After` intact and `is_transient` to \
         say whether waiting could clear it. Every attempt below is the host deciding \
         to make one. A failed turn stays `Streaming`, so the resume picks up from the \
         committed head and re-sends the identical request (FR-ERR-5).",
    );
    report.section("Run");
    report.kv(&[
        (
            "mode",
            "offline (scripted failures, no provider)".to_owned(),
        ),
        ("scenarios", records.len().to_string()),
        ("rounds per scenario", rounds.to_string()),
        (
            "simulated sleep",
            format!("{} ms per described wait", millis(SIMULATED_SLEEP)),
        ),
    ]);

    report.section("Scenarios");
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            vec![
                record.scenario.clone(),
                record.plan.join(", "),
                record.attempts.to_string(),
                if record.ok {
                    "yes".to_owned()
                } else {
                    "no".to_owned()
                },
                record.modelled_wait_ms.to_string(),
                if record.stable_requests {
                    "identical".to_owned()
                } else {
                    "MOVED".to_owned()
                },
                record.median_micros.to_string(),
                record.error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    report.table(
        &[
            "scenario",
            "injected",
            "attempts",
            "ok",
            "modelled wait ms",
            "request bytes",
            "median µs",
            "surfaced",
        ],
        &[
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Left,
        ],
        &rows,
    );
    report.bars(
        &records
            .iter()
            .map(|record| (record.scenario.clone(), u64::from(record.attempts)))
            .collect::<Vec<_>>(),
        24,
    );

    report.section("Traces");
    for record in records {
        report.lines(&format!(
            "**{}** — {} attempt(s)\n",
            record.scenario, record.attempts
        ));
        let rows: Vec<Vec<String>> = record
            .trace
            .iter()
            .map(|attempt| {
                vec![
                    attempt.index.to_string(),
                    if attempt.ok {
                        "ok".to_owned()
                    } else if attempt.retried {
                        "retry".to_owned()
                    } else {
                        "surfaced".to_owned()
                    },
                    attempt.kind.clone().unwrap_or_else(|| "-".to_owned()),
                    attempt
                        .status
                        .map_or_else(|| "-".to_owned(), |s| s.to_string()),
                    attempt
                        .retry_after_ms
                        .map_or_else(|| "-".to_owned(), |v| v.to_string()),
                    attempt.transient.to_string(),
                    attempt.waited_ms.to_string(),
                ]
            })
            .collect();
        report.table(
            &[
                "attempt",
                "result",
                "kind",
                "status",
                "retry-after ms",
                "transient",
                "waited ms",
            ],
            &[
                Align::Right,
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Left,
                Align::Right,
            ],
            &rows,
        );
    }

    report.section("What a retry costs");
    report.paragraph(
        "A retry is not free and it is not a new conversation. `request bytes: identical` \
         means the resumed turn re-sent a request whose `sha256` did not move across \
         attempts, so the provider's cache still applies and the host paid for time, not \
         for a fresh prefix. Where the policy stops, the error is surfaced with its kind \
         and status rather than flattened.",
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_policy_classifies_and_resumes_every_scenario() {
        for scenario in scenarios() {
            let record = run_scenario(&scenario, 1).await;
            let expected_ok =
                scenario.name != "exhausts-max-attempts" && scenario.name != "permanent-401";
            assert_eq!(
                record.ok, expected_ok,
                "{}: expected ok={expected_ok}, got {:?}",
                scenario.name, record.error
            );
            assert!(
                record.stable_requests,
                "{}: the resumed request moved",
                scenario.name
            );
        }
    }

    #[tokio::test]
    async fn a_permanent_error_is_not_retried() {
        let scenario = scenarios()
            .into_iter()
            .find(|scenario| scenario.name == "permanent-401")
            .expect("scenario");
        let record = run_scenario(&scenario, 1).await;
        assert_eq!(record.attempts, 1, "a 401 must not be retried");
        assert!(!record.trace[0].transient, "a 401 is not transient");
        assert!(
            !record.trace[0].retried,
            "a terminal attempt must be reported as surfaced, not retried"
        );
    }

    #[tokio::test]
    async fn an_exhausted_policy_surfaces_the_last_error() {
        let scenario = scenarios()
            .into_iter()
            .find(|scenario| scenario.name == "exhausts-max-attempts")
            .expect("scenario");
        let record = run_scenario(&scenario, 1).await;
        assert_eq!(record.attempts, scenario.policy.max_attempts);
        assert!(!record.ok);
        assert!(
            !record
                .trace
                .last()
                .expect("an exhausted policy has an attempt")
                .retried,
            "the last attempt has no successor"
        );
    }
}
