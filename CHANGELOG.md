# Changelog

All notable changes to Norte are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[semantic versioning](https://semver.org/spec/v2.0.0.html).

Every crate in the workspace shares the version in `[workspace.package]`, so one
entry describes the whole set.

## [0.0.2] - Unreleased

Draft. Not published.

### Added

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
