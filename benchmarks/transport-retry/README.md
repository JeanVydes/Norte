# transport-retry

What a `429`, a `5xx` and a dead connection cost under a host retry policy, with
the library deliberately doing nothing about them.

The contract this measures is a deliberate split. The transport does **not**
retry: a provider's answer reaches the host as a typed `ProviderError` with its
kind, its status and `Retry-After` intact, and `is_transient` to say whether
waiting could clear it. Every decision after that — wait or not, how long, how
many times — is the host's, because only the host knows whether the request is
idempotent and what it is willing to pay.

So the benchmark wraps a real `Engine` in a host policy and watches three things
for each scenario:

1. **What was surfaced** — the kind, status, `Retry-After` and the transient
   verdict of every failure, not a flattened error string.
2. **What the host paid** — attempts, and the wall-clock wait the policy chose. A
   run here sleeps a millisecond per wait instead of the provider's ask, so an
   offline run stays a benchmark and not a nap; the modelled wait is reported
   beside it.
3. **That a retry is the same request** — the resumed turn re-sends a request
   whose bytes are `sha256`-identical to the first, so the provider's cache still
   applies and the retry is not a new prefix.

Eight scenarios are scripted: a control, one 429 with `Retry-After`, two 429s
where the second is silent, three 5xx, an overloaded-then-ok, a transport
timeout, a policy that exhausts its attempts, and a permanent 401.

The model is scripted, so every scenario is deterministic and runs in CI with no
key and no network.

## Run

```sh
cargo run -p norte-bench-transport-retry -- --rounds 20
```

Offline: options are `--rounds <n>`, `--report <p>`, `--jsonl <p>`, `--help`.

## The report

- **Run** — the scenarios, the rounds, the simulated sleep.
- **Scenarios** — per scenario: what was injected, attempts, success, the
  modelled wait, whether the request bytes stayed identical, the simulated
  wall-clock and what was finally surfaced.
- **Attempts** — every attempt, in order, with the error it saw and the wait the
  host chose. An attempt followed by another is `retry`; the last attempt of a
  policy that stops is `surfaced`, even when the error is transient.

Each run also writes `<stem>.jsonl`, `<stem>-scenarios.csv`,
`<stem>-attempts.csv` and a manifest beside the markdown.

## How to read it

- **The transport's restraint is the finding.** The engine surfaces the error and
  the retry policy is visible, not hidden inside a client.
- **The modelled wait is not slept.** The run trades a millisecond for the
  provider's ask; the real ask is in the table. `simulated µs` is the wall-clock
  of the scripted run, not provider latency; it exists to catch a policy that
  loops or stalls.
- **`identical` means the retry was the same request**, so a cache still applies.
