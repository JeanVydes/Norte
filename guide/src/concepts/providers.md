# Providers

## One request, three wires

A wire adapter turns a neutral `ModelRequest` into a vendor's HTTP call and turns
the vendor's stream back into `StreamEvent`s. Three ship, plus the generic
`OpenAI`-compatible form:

| Crate | Type | Wire constant | Surface |
| --- | --- | --- | --- |
| `rustychain-openai` | `OpenAiResponses` | `WIRE_RESPONSES` (`openai.responses`) | The primary OpenAI wire. |
| `rustychain-openai` | `OpenAiChat` | `WIRE_CHAT` (`openai.chat`) | Chat Completions, and every compatible endpoint. |
| `rustychain-anthropic` | `AnthropicMessages` | `WIRE_MESSAGES` (`anthropic.messages`) | Messages. |
| `rustychain-gemini` | `GeminiGenerate` | `WIRE_GENERATE` (`google.gemini`) | generateContent, with explicit cached content. |

Each is constructed the same way:

```rust,ignore
let model = OpenAiChat::new(reference, profile, capabilities)?;
```

`reference: ModelRef` says who answers, `profile: VendorProfile` says where and
how, `capabilities` says what it can do. `with_client(HttpClient)` replaces the
transport, `profile()` reads it back, and `request_body(&ModelRequest)` shows the
exact JSON a request will produce — the fastest way to debug a wire.

## Native presets

```rust,ignore
use rustychain_anthropic::presets::anthropic;
use rustychain_gemini::presets::gemini;
use rustychain_openai::presets::openai;

let a = openai("gpt-5", key, capabilities)?;         // Responses wire
let b = anthropic("claude-sonnet-4-5", key, capabilities)?;
let c = gemini("gemini-2.5-pro", key, capabilities)?;
```

Each crate also exports `<vendor>_profile(api_key) -> Result<VendorProfile>` if
you want the profile without the adapter.

## Any OpenAI-compatible endpoint

That is a `VendorProfile`, not a preset:

```rust,ignore
use rustychain_core::{ModelId, ModelRef, ProviderId, WireFamily};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};

let provider = ProviderId::new("groq")?;
let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, base_url)?
    .with_auth(AuthScheme::bearer(key));
let model = OpenAiChat::new(
    ModelRef::new(provider, ModelId::new(model_id)?),
    profile,
    capabilities,
)?;
```

OpenRouter, Groq, Together, vLLM, llama.cpp and the rest go through that door.
`VendorProfile` also has `with_header(name, value)` and `with_quirk(quirk)` for
the small ways a compatible endpoint is not quite compatible.

`AuthScheme` is `None`, `Bearer`, `Header { name, value }` or `Query { name,
value }` — the four shapes the field actually uses. Secrets are wrapped in
`Secret`, which does not print itself.

## Transport

`rustychain-http` owns the network and is conservative on purpose:

- **Only connection establishment is retried.** Nothing was sent, so the provider
  cannot have started working and the retry is invisible. A timeout is *not*
  retried even though it is a transport failure — by then the request may be
  running, and a generation must never run twice.
- **A provider's answer is never retried by the transport.** A rate limit, a
  quota, a `5xx` reaches the host as an error with `retry_after` intact, because
  whether to wait is application policy.
- **Timeouts are explicit**: `connect_timeout`, `request_timeout`, and
  `stream_idle_timeout` — the last bounds the silence *inside* an event stream, so
  a provider that stops sending mid-answer fails instead of hanging a turn
  forever.

`HttpClient`, `TransportPolicy`, `SseDecoder` and `parse_retry_after` are public
if you want them directly.

## What is missing, honestly

Each adapter covers roughly 95 % of its wire's documented surface. The missing
part is small and named: the Realtime API (out of scope), `logprobs` (opt-in,
per-token and floating point, which would break the `Eq` invariant every domain
part relies on), and unary `stream: false` delivery (`finish()` already covers
blocking callers).

Two rules the adapters do keep:

- **A modality a wire cannot express is a typed error**, never a silent drop.
- **A documented block with no typed neutral form is kept verbatim** — as a
  `ProviderToolCall`, `ProviderToolResult` or `Extension` part — and replayed
  unchanged, so nothing the provider said is lost on the way back out.
