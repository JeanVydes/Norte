# Changelog

All notable changes to Norte are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[semantic versioning](https://semver.org/spec/v2.0.0.html).

Every crate in the workspace shares the version in `[workspace.package]`, so one
entry describes the whole set.

## [0.0.1]

First release.

- `norte-core` — the neutral domain model, the streaming engine, policies,
  approval, layers, observers, capabilities, usage and the port traits (`Store`,
  `BlobStore`), with in-memory implementations of both.
- `norte-http` — shared transport: `VendorProfile`, auth schemes, transport
  policy, SSE decoding and provider-file media.
- `norte-openai` — the `Responses` and `Chat Completions` wires, the
  `openai` preset, and `OpenAiFiles`.
- `norte-anthropic` — the `Messages` wire, the `anthropic` preset, and
  `AnthropicFiles`.
- `norte-gemini` — `generateContent` with explicit cached content, the
  `gemini` preset, and `GeminiFiles`.
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

Each adapter covers roughly 95 % of its wire's documented surface; what is
missing is named in the README. There is no model catalogue, no price table and
no agent loop: capabilities are declared by the host, usage is reported by the
provider, and the loop is application code.
