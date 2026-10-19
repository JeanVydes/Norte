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

Each adapter covers roughly 95 % of its wire's documented surface; what is
missing is named in the README. There is no model catalogue, no price table and
no agent loop: capabilities are declared by the host, usage is reported by the
provider, and the loop is application code.
