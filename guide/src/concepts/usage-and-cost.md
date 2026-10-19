# Usage and cost

Every message the engine commits carries an origin with a `Usage`. It is the
provider's report of what the request cost in tokens, kept as reported.

```rust,ignore
use norte_core::{usage_of, Message};

let usage = usage_of(&message);
println!(
    "in {:?} out {:?} cached {:?}",
    usage.input_tokens, usage.output_tokens, usage.cache_read_tokens
);
```

## The fields

| Field | Type | Meaning |
| --- | --- | --- |
| `input_tokens` | `Option<u64>` | Prompt tokens, cache reads included on `OpenAI`-compatible wires. |
| `output_tokens` | `Option<u64>` | Tokens the model produced. |
| `reasoning_tokens` | `Option<u64>` | Reasoning tokens, where the provider separates them. |
| `cache_read_tokens` | `Option<u64>` | Tokens served from the provider's cache. |
| `cache_write_tokens` | `Option<u64>` | Tokens written into it. |
| `audio_input_tokens` / `audio_output_tokens` | `Option<u64>` | Audio counters, where the provider has them. |
| `extension` | `BTreeMap<String, u64>` | A counter with no neutral field, kept as a number. |
| `vendor` | `BTreeMap<String, Value>` | Anything else the payload carried, kept verbatim. |

`total_tokens()` adds what is present. Counters are `Option` because a provider
that does not report one must not be read as reporting zero.

## Why there is no `cost` field

**The engine never prices anything.** There is no rate table in this repository,
and there will not be one, because it would be wrong the moment a request is
routed somewhere else. A provider that reports a price in its usage payload is
reporting a fact about that request; a price table in the library would be a
guess about the next one.

So the price, when the provider sends one, lands in `Usage::vendor` under the key
the provider used — `cost`, on `OpenRouter`. Read it if you want it:

```rust,ignore
fn reported_cost(usage: &norte_core::Usage) -> Option<f64> {
    usage.vendor.get("cost").and_then(serde_json::Value::as_f64)
}
```

That is the whole meter. Summing that field over the requests of a session gives
a bill the library never had to guess.

## Counting requests is the point

An agent's cost is not one request; it is the transcript re-sent once per
iteration, and the transcript grows by every tool result. A three-task coding
session in this repository's own benchmark (`benchmarks/agent-cost`) shows the
shape: the three tasks spent 9, 10 and 13 requests, and 14 of 41 tool calls were
refused — most from guessing at the `command` schema — and every refusal was a
billed request of its own.

Two things follow:

- **Log `RequestReconstructed`.** It fires once per model request, including
  every round of a tool loop. A turn's outcome shows only the last answer, while
  every iteration committed and billed a message of its own.
- **Cache reads are inside `input`** on `OpenAI`-compatible wires, so a "hit
  rate" is `cache_read / input`, and what it was worth is already in the
  provider's number. Print it beside the bill; never derive a bill from it.

## Reading usage out of a stream

`Usage` is not an `EngineEvent` — the stream carries the tokens, the store
carries the bill. The pattern:

```rust,ignore
// after the next RequestReconstructed, or at the end of the turn:
for message in engine.history(session.id).await? {
    if let Some(origin) = message.origin.as_ref() {
        let _ = &origin.usage;      // this request's counters
    }
}
```

`MessageOrigin` also carries which model produced the message, so a session that
switched models mid-conversation keeps a truthful per-request record.

## `Usage` is part of the domain

Because it is a domain type, usage participates in export and re-import: a
restored session reports the same counters it reported at the time, which is what
makes a bill reproducible after a restart.
