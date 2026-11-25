# Norte

Norte is a Rust library for calling language models through a shared API. It
provides typed messages and tool calls, provider adapters, capability checks,
and streams of model events.

Norte is not an agent runtime. It does not create sessions, manage conversation
history, dispatch tools in a loop, or persist application state. The host
application decides how those pieces fit together.

## What Norte provides

- A provider-neutral [`Model`](https://docs.rs/norte-core/latest/norte_core/trait.Model.html)
  interface and request types.
- OpenAI, Anthropic, and Gemini adapters.
- Typed capabilities and request validation.
- Streaming events, tools, media types, and opt-in integration helpers.

## Where to go

- New to the API: [Getting started](getting-started.md).
- Public crates: [Crates](reference/crates.md).
- Full API: [Rustdoc](https://docs.rs/norte/).
