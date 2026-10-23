# Changelog

All notable changes to Norte are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[semantic versioning](https://semver.org/spec/v2.0.0.html).

Every crate in the workspace shares the version in `[workspace.package]`, so one
entry describes the whole set.

## [0.0.2] - Unreleased

Draft. Not published.

### Fixed

- A shell `command` no longer has to name its interpreter, and a `write` no
  longer has to send `overwrite`: they default to `sh` and `false`. Both were
  required only by the schema, so the natural call failed and models guessed.
- An Anthropic request that asks for a reasoning token budget now sends
  `thinking: {type: enabled, budget_tokens}`. The adapter only sent the
  current `output_config.effort` levels, so a model that advertises a budget
  (Claude Haiku 4.5) ran without the requested thinking.
- Anthropic's `input_tokens` now includes the tokens served from and written
  to the prompt cache, as the other wires already did. A cache share cannot
  exceed 100%, and the prompt total the engine records matches what the
  provider billed.
- A provider that puts its retry delay in the error body instead of a
  `Retry-After` header is now honored. Gemini's `google.rpc.RetryInfo` detail
  sets `retry_after`, so a host waits for the quota window to reopen instead of
  exhausting its attempts against a closed one. The streaming decoder reads the
  same detail when the error arrives as an event.
- Tool calls on Gemini no longer fail with `Invalid JSON payload`: the adapter
  rewrites every schema into the OpenAPI subset the API accepts. It turns
  `type: [T, "null"]` into `nullable`, `oneOf` into `anyOf`, `const` into an
  `enum`, `examples` into `example`, and drops `$schema`,
  `additionalProperties` and the other JSON Schema keywords Gemini does not
  know. Structured output's `responseSchema` goes through the same rewrite.
- The digest the engine records described the request as it was reconstructed,
  but a layer may alter the request before the model sees it, so the audit
  trail did not match the wire. The engine now digests the request that reached
  the model, and the `RequestReconstructed` event reports that digest
  (FR-ENG-4).
- A turn that started without an input never drained the steering queue: the
  queue is drained when a turn appends an input, so a message queued while the
  session rested waited for the next turn that had one. The engine now drains
  it when the session is idle, which is the only moment the head owes no tool
  result; a parked session keeps waiting, because the messages must not come
  between a tool call and its result.

### Removed

- The steering queue of the session. `Session::queue`, `Commit::queue` and the
  `queue` of the exported envelope are gone, and so is `Engine::steer`. A
  message written to the session while a turn runs could not be committed
  without racing that turn, so it would have had to wait for the next turn
  instead of reaching the request already in flight. Steering now goes through
  the turn interceptor, which the engine consults before the turn's input,
  before every model request and before the turn ends (FR-ENG-3, FR-ENG-10).

### Added

- Turn interceptors. A host may register an interceptor the engine consults at
  the safe points of a running turn: before the turn's input, before every
  model request, and before the turn ends. The interceptor decides whether the
  turn goes on, adds messages to the session's history, or stops with a reason;
  the messages it adds are chained onto the session's head and committed by the
  engine, so they are durable, part of the request digest, visible to the
  policies and free of a version race. The steering queue cannot do this: it is
  a session field, so a message written to it while a turn runs cannot be
  committed without racing that turn (FR-ENG-3, FR-ENG-10).
- Reasoning efforts `XHigh` and `Max`, matching the levels OpenAI and Anthropic
  now document. The OpenAI Responses and Chat adapters serialize them as
  `xhigh` and `max`, and the Anthropic adapter sends `output_config.effort`.
  Gemini, whose ceiling is `HIGH`, maps both to `HIGH`.
- Turn cancellation. `EngineStream::cancel()` asks the turn to stop, and dropping
  the stream cancels it. The token reaches every running tool through
  `ToolContext` (`cancellation()`, `cancelled()`, `is_cancelled()`), including a
  call started early. Completed parts are committed, the assistant message is
  `Interrupted`, and the session stays resumable. A turn cancelled while tools
  run commits no tool result and stays `AwaitingTool` (FR-STREAM-3).
- Host context for a tool call: `ToolMetadataProvider` and
  `ToolContext::metadata` let the host attach tenant, actor or trace data to a
  call. The provider is consulted immediately before each execution — the first
  attempt and a resumed park alike — and nothing is written to the graph or the
  wire (FR-TOOL-7).
- Media inside a tool result on every wire that can express it; a wire whose
  tool-result field is text-only refuses the media with a typed capability error
  instead of dropping it (FR-TOOL-3).
- Provider file stores: `OpenAiFiles`, `AnthropicFiles` and `GeminiFiles`
  implement the host-wired `ProviderFiles` port — upload bytes, download them
  back — so an expired `ProviderFile` reference is revived from its blob rather
  than dropped. `norte-http` gains multipart request support and
  `HttpMedia::with_provider_files` (FR-PART-4, FR-PART-5, FR-ADP-4).
- Nine benchmarks under `benchmarks/` — `agent-cost`, `cache-hit`, `latency`,
  `long-context`, `roundtrip-fidelity`, `schema-friction`, `token-counting`,
  `tool-loop` and `transport-retry` — on a shared `norte-bench-support` crate that
  writes a markdown report, a JSONL trail, CSV tables and a run manifest per run.
  Live benchmarks wait `BENCH_PAUSE_MS` (2 s by default) before each request so an
  asynchronous provider prefix cache can publish.

### Changed

- Provider tools stay untyped: `ToolSpec::declaration` is an opaque
  `serde_json::Value`, re-emitted verbatim.
- Retries and rate limiting are host policy. The engine does not retry; a
  transient error reaches the host with `Retry-After` and `is_transient`. Unary
  `stream: false` and the Realtime API are recorded as out of scope.
- `cache-hit` is a benchmark, not a cookbook recipe; the cookbook's rate-limit
  example is the retry-policy recipe.
- Examples use `deepseek/deepseek-v4.1-flash-1017`; the provider is chosen by the
  router unless the run pins one.

### Documentation

- The effect boundary between the engine and the host is stated in the guide,
  with a host-side idempotency recipe in
  `examples/cookbook/examples/idempotent_tool.rs`.

### Fixed

- `read` and `edit` refuse a FIFO, socket or device instead of opening it. A
  FIFO with no writer blocked `File::open` forever, and the blocking read was
  not cancellable, so one tool call could wedge the whole turn.
- Policy precedence no longer depends on registration order. A `Transform`
  after a `RequireApproval` used to replace the approval and run the call
  without a human; approval now wins in either order, carries the last rewritten
  arguments, and the engine runs exactly those. `ApprovalRecord::arguments`
  stores them for the host to display and for restart-resume.
- A denial with an empty reason now reports the plain "the operator denied this
  call" message instead of a dangling colon.
- An `edit` failure now names the most common escaping mistake: a search that
  contains a literal `\n`, `\r` or `\t` while the file has the real character,
  or the reverse, gets a hint in the report. An ambiguous exact match also
  reports occurrences rather than "places", saying that same-line and
  overlapping hits count separately, so the count cannot be mistaken for a line
  count (FR-TOOL-EDIT-4).

## [0.0.1]

First release.

- `norte-core` — the neutral domain model, the streaming engine, policies,
  approval, layers, observers, capabilities, usage and the port traits (`Store`,
  `BlobStore`), with in-memory implementations of both.
- `norte-http` — shared transport: `VendorProfile`, auth schemes, transport
  policy, SSE decoding and provider-file media.
- `norte-openai` — the `Responses` and `Chat Completions` wires, and the
  `openai` preset.
- `norte-anthropic` — the `Messages` wire, and the `anthropic` preset.
- `norte-gemini` — `generateContent` with explicit cached content, and the
  `gemini` preset.
- `norte-tools` — `read`, `write`, `edit` and `command`, the default host tools.
- `norte-media` — image preprocessing (`ImagePreprocessor`, `ImageOutput`).
- `norte-mcp` — Model Context Protocol client and server, with remote tools as
  `AnyFunction`s.
- `norte-testing` — scripted models, fixture replay, request probes and
  conformance suites, for tests only.
- `norte-macros` — `#[tool]`, `declare_function!` and `declare_tool!`.
- `norte` — the facade: one dependency, everything re-exported.
- Token-level log probabilities (`logprobs`): opt-in per turn, gated on
  `Feature::Logprobs`, mapped on the OpenAI Chat, OpenAI Responses and Gemini
  wires, and delivered as an informational `Logprobs` delta the host may keep.
  The library never persists them.

Each adapter covers roughly 95 % of its wire's documented surface; what is
missing is named in the README. There is no model catalogue, no price table and
no agent loop: capabilities are declared by the host, usage is reported by the
provider, and the loop is application code.
