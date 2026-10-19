# Requirements

The normative contract behind Norte: what the library promises, and where each
promise is implemented and checked. The ids below are the ones cited in the code
and in the CI job names, so a requirement can be followed from this page to the
code that enforces it, and from that code to the test or job that fails when it
breaks.

This page states *what*; the concept pages explain *why*. Each requirement names
the code it lives in when that code cites the id, and each section closes with
the suites and CI jobs that check it. A requirement without a citation is
enforced by its section's suite as a whole.

Ids are stable: they are referenced from source comments and workflow names, so
they are not renumbered when wording is tightened.

## 1. Introduction

### 1.1 Purpose

Norte is an open-source Rust framework for constructing production agents on the three major model providers — Anthropic, OpenAI (and wire-compatible vendors) and Gemini. It supplies a durable conversation graph, a stream-first execution model, strong typing end-to-end, provider-native tool support and a resilient human-approval system. The core remains free of external storage backends, concrete tools, user interfaces, vector memory and multi-agent routing; those concerns live in additive crates. It does ship ready-to-use in-memory `Store` and `BlobStore` implementations, so an application runs with no external service while remaining free to plug in its own.

### 1.2 Scope

**In scope**

- Domain model, typed identifiers, conversation graph and sessions.
- Streaming pipeline as the sole interaction primitive.
- Capability model that resolves and enforces model features at runtime from host-supplied data.
- Tool foundation (typed declarations, registry and execution contract) in core, plus a production-grade command executor in the default tools suite.
- Optional, host-configured media preprocessing (resize, transcode) as a building block.
- Composable layers, pure policies and typed observers.
- Adapters that expose the documented public surface of Anthropic Messages, OpenAI Responses (primary) / Chat Completions (compatibility) and Gemini generateContent — every feature a capability descriptor advertises, with the ceiling §12 states.
- Storage and blob contracts, with ready-to-use in-memory implementations for development and single-process deployments.
- Durable human-approval records with expiry, edits, grants and audit.
- Deterministic, prefix-stable history reconstruction that keeps provider prompt caches valid.

**Out of scope for core**

- Concrete database or object-storage backends.
- User-facing UIs or HTTP servers.
- Vector memory / RAG pipelines.
- Multi-agent orchestration and routing.
- MCP client/server implementations beyond the tool foundation.
- Any derived or compacted view of the conversation, or any other higher-level interpretation of history. Core models exactly one node kind, `Message`; hosts that want derived views express them with ordinary messages and host-owned metadata, never with a new node kind.

### 1.3 Audience

Library authors integrating providers, agent-framework developers, and operators who require deterministic, restart-safe agent behaviour under production load.

## 2. Quality Bar & Principles

### 2.1 Quality Bar

“Production-ready” means every property below is testable and covered by automated tests.

- No panic on any external input: wire data, tool output, stored data. Parsers are fuzzed.
- Bounded memory per stream and per session; no unbounded channels.
- Every public async operation is cancellation-safe and leaves resumable state.
- A process may die at any instant without corrupting a session or losing committed work.
- No silent degradation: an unsupported feature is a typed error or an explicit, logged fallback.
- Provider behaviour is pinned by recorded fixtures and rechecked by live smoke tests.
- Provider prompt caches are kept hitting across turns and across branch/rewind: reconstruction is byte-equivalent or prefix-stable (Section 8).
- Stable public API under semantic versioning, `#![forbid(unsafe_code)]`, a declared MSRV.
- Complete coverage of the documented public surface of Anthropic, OpenAI Responses/Chat Completions and Gemini for the features the capability model advertises.

### 2.2 Principles

- **Stream-first.** Every model interaction is an event stream; blocking results are derived by collecting the stream.
- **Stateless executor, durable state.** The agent holds configuration; the store holds everything else.
- **One transition, one atomic commit.** No multi-write transitions. A commit either succeeds completely or leaves the previous state intact.
- **Append-only history.** Branching and rewinding add nodes or move a pointer; they never edit or delete existing nodes.
- **Prefix-stable reconstruction.** Rebuilding history for a request is a deterministic, append-only function of the graph, so a provider’s prompt cache keeps hitting. Cache is money: there is no override.
- **Core depends on nothing concrete.** No HTTP client, database driver, vendor SDK or concrete tool appears in core.
- **Capabilities are data.** The library asks what a model can do instead of assuming, and ships no model catalog.
- **The engine never prices.** No crate carries a rate, a price table or a cache-discount formula: money reaches the host exactly as the provider reported it (`Usage::vendor`, FR-ADP-5) and the host decides what to do with it. Which upstream serves a request is the router's decision, request by request, so a rate written into the library would be the list price of a route that may never answer; a cost harness over `Usage` therefore sums the payload's own `cost` and prints the token counters beside it rather than deriving a bill the library would have to keep current.
- **Vendor detail stays in adapters.** The neutral model carries opaque continuity tokens, never vendor fields.
- **Decide once, persist, never re-decide.** Policy and approval outcomes are stored together with the policy identity.
- **Typed end to end.** Typed identifiers, typed tool arguments and outputs, exhaustive matching, no stringly-typed control flow.
- **Full provider fidelity, with a named ceiling.** Adapters expose every documented capability of each provider's chat surface; the surface that is deliberately out of scope is named rather than implied away (the Realtime API), and every other gap is a typed error or a value kept verbatim, never a silent omission.

**Verified by:** CI jobs **Format Check**, **Clippy**, **MSRV**, **Documentation** and **Security Audit**; the properties are asserted across the workspace suites.

## 3. Architecture

### 3.1 Crates

| Crate | Responsibility | May depend on |
| --- | --- | --- |
| `norte-core` | Domain model, engine, stream accumulator, capability model, `Model` / `Store` / `BlobStore` traits with in-memory implementations, tool foundation contracts and registry, layers, policies, observers, events, history reconstruction. | serde, schema generation, tokio (runtime, time, sync), futures, tracing. No HTTP, no database, no vendor, no concrete tool. |
| `norte-http` | Shared transport: SSE plumbing, auth providers, `TransportPolicy` (reconnect budget and timeouts), `Retry-After` parsing, secret types, vendor profiles, quirk flags. | core, reqwest, url |
| `norte-anthropic` | Anthropic Messages wire, all documented features. | core, http |
| `norte-openai` | OpenAI Responses wire (primary), Chat Completions wire with quirk flags, vendor presets. | core, http |
| `norte-gemini` | Gemini generateContent wire, full feature surface. | core, http |
| `norte-tools` | Default tool suite. Ships the production-grade command executor, the file executors (read, write, edit) and their host-facing schemas. | core |
| `norte-media` | Optional media preprocessing: host-configured image resize / re-encode. | core |
| `norte-testing` | Scripted model, fixture replayer, request probe, conformance suites. | core |
| `norte-mcp` | Model Context Protocol in both directions: `McpClient` performs the handshake and wraps every remote tool as one host tool named `mcp__<server>__<tool>`, so per-tool schemas, approval policy and audit records survive; `McpServer` publishes a host `ToolRegistry` over a transport. Stdio and in-process transports ship. | core |
| future crates | `norte-store-*`, `norte-tools-*`. | core |

### 3.2 Dependency Rule

**NFR-ARCH-1.** Dependencies point toward core only. Adapters never depend on each other. A vendor that speaks a supported wire format is a preset inside that wire’s crate, never a new crate. CI inspects the dependency graph of core and fails if any HTTP, database, vendor or concrete-tool crate appears.

### 3.3 Layer View

Host application → optional higher-level crates (MCP, extra tools) → engine (core) → layers / policies / observers → adapter (anthropic | openai | gemini) → http transport → provider.

**Verified by:** CI job **Dependency Rule (NFR-ARCH-1)**, which runs `scripts/check-dependencies.sh`.

## 4. Domain Model

### 4.1 Typed Identifiers

**FR-ID-1.** The following are distinct newtypes, never bare strings or integers: `NodeId`, `SessionId`, `CallId`, `GenerationId`, `ApprovalId`, `BlobId` (content hash), `ProviderId`, `ModelId`, and `ModelRef` (a provider and model pair). They implement `Display`, `FromStr` and serde, and cannot be mixed by accident.

**FR-ID-2.** `CallId` is assigned by the engine. The vendor’s own call id, when present, is kept inside the continuity token, because some vendors issue none (Gemini) and others reuse ids across turns.

### 4.2 Roles

**FR-ROLE-1.** The graph stores five semantic roles: `System`, `Developer`, `User`, `Assistant`, `Tool`. Wire role names exist only in adapters.

**FR-ROLE-2.** `System` and `Developer` messages may appear anywhere in a conversation, supporting mid-run instruction updates. How an adapter represents them is its documented role policy.

**Where it lives:** `norte-anthropic/src/messages/request.rs` (`build_messages`), `norte-gemini/src/generate/request.rs` (`instruction_count`), `norte-openai/src/chat/request.rs` (`push_user`), `norte-openai/src/responses/request.rs` (`push_message`).

**FR-ROLE-3.** All results for one assistant turn live in a single `Tool` message, in call order.

### 4.3 Parts (Content)

A message is a role plus an ordered list of parts.

| Part | Content | Notes |
| --- | --- | --- |
| Text | text, citations | A citation is a provider-neutral reference to a URL, document or range. |
| Reasoning | visibility (Full, Summary, Redacted, Omitted), text, continuity | For display. Replay is governed by continuity, not by this text. |
| Image, Audio, Video, Document | media source, MIME type, optional metadata (duration, dimensions, transcript) | Inputs and model-generated outputs. |
| ToolCall | call id, name, JSON arguments, continuity | Executed by the host. |
| ToolResult | call id, parts (text and media), optional structured JSON, error flag | May be multimodal. |
| ProviderToolCall, ProviderToolResult | tool kind, structured input and output, citations, continuity | Executed by the vendor (search, code execution, …). |
| Refusal | text | Distinguished from ordinary text where the vendor distinguishes it. |
| Extension | namespace, JSON | Forward compatibility: unknown wire blocks round-trip unchanged. |

A media source is one of: `Blob(BlobId)`, `Url`, `ProviderFile` (vendor file id with provider and expiry), or `Inline` bytes below the inline threshold.

**FR-PART-1.** All public enums that mirror external formats are `#[non_exhaustive]`. A wire block the library does not model becomes an `Extension` part and is returned to the same vendor unchanged.

**FR-PART-2.** Media above the inline threshold (default 64 KiB, host-configurable) shall be stored through `BlobStore` and referenced by id. Graph nodes stay small and cheap to load.

**FR-PART-3.** Model-generated media arrives as stream chunks and is written to `BlobStore` when its part ends, unless a host-supplied preprocessor reshapes it first.

**Where it lives:** `norte-core/src/engine.rs` (`preprocess_history`), `norte-testing/tests/media.rs`.

**FR-PART-4.** A `ProviderFile` reference that has expired shall be re-uploaded from the blob automatically when the blob still exists; otherwise the request fails with a typed error naming the part.

**Where it lives:** `norte-core/src/domain/media.rs` (`deserialize`), `norte-core/src/media_adapter.rs` (`ingest`, `resolve`), `norte-http/src/media.rs` (`new`).

**FR-PART-5.** Model-generated media that a vendor delivers as a `Url` or `ProviderFile` rather than stream chunks shall be ingested into `BlobStore` before the assistant message is committed, so the graph never depends on an ephemeral, expiring URL.

**Where it lives:** `norte-core/src/media_adapter.rs` (`resolve`), `norte-http/src/media.rs` (`new`), `norte-testing/tests/media.rs`.

**FR-PART-6.** Media preprocessing (resize, re-encode, transcode) is an optional, host-configured building block. Core defines the contract only; it ships no policy and no model limits. Preprocessing must be idempotent so applying it on every turn does not shrink content repeatedly.

### 4.4 Continuity Tokens

Some vendor state must be sent back unchanged for the next request to be valid: Anthropic thinking signatures, Gemini thought signatures (required for function calling on certain Gemini models), OpenAI and xAI encrypted reasoning items, and encrypted results of vendor-executed tools.

A continuity token is an opaque envelope: wire family, scope (exact model, model family, provider, or any), schema version, and an opaque payload. Core never reads the payload. Tokens attach to `Reasoning`, `ToolCall`, `Text` and `ProviderTool*` parts, and to whole messages (for vendor item ids).

**FR-CONT-1.** All interpretation lives in adapter methods: building a token from wire data, merging streamed fragments into a token, and deciding replay. Replay takes the target `ModelRef` and the history and returns a plan per token: `Replay`, `Drop`, `Downgrade(part)` or `Block(reason)`.

**FR-CONT-2.** Default rules: same exact model → `Replay`; same provider, different model → adapter table (default `Drop`, with `Block` where dropping would make the vendor reject the request); different provider → `Drop`. A part whose content was edited or branched invalidates its token.

**Where it lives:** `norte-anthropic/src/continuity.rs` (`replay`), `norte-gemini/src/continuity.rs` (`replay`), `norte-openai/src/continuity.rs` (`replay`), `norte-testing/src/model.rs` (`replay`), `norte-testing/tests/export.rs` (`a_forked_conversation_moves_only_the_cache_key`).

**FR-CONT-3.** Tokens often arrive as deltas. The stream carries opaque fragments with `Append` or `Replace` semantics; the adapter’s merge method defines the result.

**FR-CONT-4.** A conversation switched mid-tool-loop to a model that requires tokens the history cannot supply shall fail the replay with `Block`, never send a request known to be rejected.

**Where it lives:** `norte-anthropic/src/continuity.rs` (`replay`), `norte-gemini/src/continuity.rs` (`replay`), `norte-openai/src/continuity.rs` (`replay`), `norte-testing/src/model.rs` (`replay`), `norte-testing/tests/export.rs` (`a_forked_conversation_moves_only_the_cache_key`).

**FR-CONT-5.** Tokens are treated as sensitive: never logged and never placed in events (length only). The export envelope carries them as part of the transcript — stripping them would change the wire a restored session produces (FR-EXP-2, FR-EXP-4) — and the destination’s replay table decides what may be replayed (FR-CONT-2). A host that must not ship them strips them from the envelope before serializing, accepting an explicit, lossy export.

**FR-CONT-6.** The engine operates statelessly with respect to the vendor. It never depends on vendor-side conversation state (response ids, stored threads); where a vendor offers it, the adapter opts out (for OpenAI Responses: not stored, encrypted reasoning requested).

### 4.5 Messages, Nodes, Sessions

**Message.** Role, parts and, for assistant messages, an origin: model reference, generation id, finish reason (`Stop`, `ToolCalls`, `Length`, `ContentFilter`, `Refusal`, `Cancelled`, `Other`), usage and a status (`Complete`, `Interrupted`, `Truncated`).

**Usage.** Input, output, reasoning, cache read, cache write, audio input and audio output tokens, each optional; `extension`, a map of vendor counters the neutral vocabulary has no field for; and `vendor`, the vendor's usage object kept verbatim minus every counter an adapter claimed. A price (`cost`, `cost_details`), a flag (`is_byok`) or a field this vocabulary has never heard of therefore survives the parse instead of dying there. Claiming is what removes a field, so a counter an adapter stops mapping appears in `vendor` rather than being lost, a value that is not an unsigned integer is never coerced into one, and an object every one of whose fields was claimed is pruned so a fully mapped payload carries no map at all (FR-ADP-5).

**Node.** id, optional parent, kind (`Message` only in core), message, `created_at`, host-owned metadata, and a `schema_version`. A message additionally carries a typed **cache class** (`Critical`, the default, or `Ephemeral`); an ephemeral message is materialized after the cache-critical prefix (Section 8). Ephemerality is per message: a part that must stay out of the prefix travels in an ephemeral message.

**Session.** id, `head` (leaf of the active branch), phase, version (concurrency token, +1 per commit), turn counter, a queue of steering messages, approval grants, host metadata, timestamps.

**Commit.** `session_id`, `expected_version` (`None` = create), nodes (parent first), new `head`, phase, turn, queue, grants, optional metadata.

**FR-GRAPH-1.** The graph is a tree. Branches share a parent. Nodes belong to no session, so sessions may share ancestors.

**Where it lives:** `examples/cookbook/examples/branching.rs`.

**FR-GRAPH-2.** Nodes are immutable. `metadata` is host-owned and returned unchanged, except inside the reserved `norte.` namespace, which is the engine's own and where it writes exactly two keys: `norte.turn_options` (the per-turn overrides the node was generated under, FR-ENG-7) and `norte.partial_tools` (the results a parked message had already produced, FR-APPR-8). A host keeps its metadata outside that prefix; the rest of what the engine records about a node — model, usage, finish reason, generation id and request digest — lives in the node's own fields, not in its metadata.

**Where it lives:** `norte-testing/tests/media.rs` (`an_expired_provider_file_is_revived_once_and_the_prefix_stays_stable`), `norte-testing/tests/reconstruction.rs` (`a_branch_from_an_ancestor_reproduces_the_shared_prefix_byte_for_byte`).

**FR-GRAPH-3.** Persisted types evolve by additive change only; readers ignore unknown fields and the engine returns a typed error for an unknown phase instead of guessing.

**FR-GRAPH-4.** Core defines exactly one node kind, `Message`. Core introduces no other node kind and never compacts, reorders or drops stored messages.

**Where it lives:** `norte-core/src/domain/conversation.rs`.

**Verified by:** `norte-core` suite (`domain/`, `stream.rs`); byte identity in `norte-testing` (`tests/export.rs`, `tests/reconstruction.rs`).

## 5. Capability Model

Capabilities are data. Before every request the engine consults the model’s capability descriptor and the requested features; mismatches become typed errors or explicit, logged fallbacks. The framework ships **no model catalog**: descriptors are host-supplied.

**FR-CAP-1.** An adapter accepts a host-supplied capability descriptor per model, static or dynamically refreshed (typically JSON). A descriptor carries: supported modalities (text, image, audio, video, document), reasoning control levels, tool kinds (function, provider-executed, provider-schema), streaming granularity, max context, max output, structured-output modes, cache support, and provider-specific flags. The library maintains no built-in list of models.

**FR-CAP-2.** A reasoning request is typed and resolved against the descriptor. Models that cannot disable thinking report the limitation; the engine never silently drops the request.

**FR-CAP-3.** Media parts are validated only against the limits the host configured for that model (accepted MIME types, maximum bytes, maximum count per request). An empty or absent limit means **unrestricted**; the engine never invents limits.

**FR-CAP-4.** Unsupported tool kinds or parameters produce a typed `Error::Capability` naming the model and the missing feature. Validation applies to the **effective** request configuration after the per-turn merge (FR-ENG-7), so an override the model cannot satisfy is rejected for that turn rather than silently ignored or applied globally.

**FR-CAP-5.** The engine enforces exactly what the host configured and nothing more. Feature toggles are a closed, typed set; structured-output modes are a closed, typed set; there is no free-form flag bag.

**FR-CAP-6.** A descriptor is assembled with a typed, fluent builder, or parsed from host JSON. The builder accepts closed enums and typed values only, never strings, so a misspelled capability cannot enter the descriptor.

**FR-LP-1.** Token-level log probabilities are opt-in per turn: the turn asks for a count, and the engine rejects that turn with a typed error when the model does not declare `Feature::Logprobs`, or when the count exceeds the neutral cap of 20. Both failures are raised before the request is built, so a model that cannot report probabilities is never asked for them. Every other turn of the same session is unaffected.

**Where it lives:** `norte-core/src/capability.rs` (`check_generation`), `norte-core/src/engine.rs` (`TurnBuilder::logprobs`), `norte-core/src/model.rs`.

**Verified by:** `norte-core` (`capability.rs`) and `norte-testing` (`tests/engine.rs`, `tests/turn_options.rs`).

## 6. Streaming Pipeline

Streaming is the only primitive. A blocking call is obtained by collecting the stream.

**FR-STREAM-1.** The engine yields a typed event stream: text deltas, reasoning deltas, refusal deltas, tool-call argument deltas, media chunk boundaries, provider-tool events, finish reasons, safety ratings, usage updates and continuity fragments. A refusal the vendor streams the way it streams text (OpenAI Chat and Responses) accumulates into one `Refusal` part. Streamed media is opened with `MediaStart`, filled with `MediaChunk` boundaries and may carry a late `MediaMetadata` event that settles the descriptive fields (an audio transcript, a vendor media id) once they are known, before the closing chunk. A vendor that reports per-candidate safety ratings (Gemini `safetyRatings`) surfaces them as a `SafetyRatings` event whose neutral `SafetyRating` values keep `category`, `probability`, `severity` and `blocked` typed while every other vendor field (numeric scores included) stays in an extension map, so nothing is lost and the `Eq` invariant holds.

**Where it lives:** `norte-core/src/stream.rs` (`close_tool_call`).

**FR-STREAM-2.** Stream accumulation is bounded; partial parts are held in a fixed-capacity buffer and flushed to the store only on part completion or explicit checkpoint.

**FR-STREAM-3.** Cancellation of the stream is safe: the engine commits any completed parts, marks the assistant message `Interrupted`, and leaves the session resumable.

**FR-STREAM-4.** Tool calls may begin execution as soon as their arguments are complete, before the assistant message has finished streaming, under the control of the host’s tool policy. A wire whose tool blocks have an explicit boundary signals completion with a `ToolCallDone` stream event. When the host enables early tool execution (`EngineConfig::early_tool_execution`), the engine evaluates the tool policy against the assistant message as known so far and starts the call in a background task that runs while the remainder of the message streams; the results feed the normal tool turn without re-evaluation. Calls that need approval, are denied or are rate limited are always deferred to the normal path, and a call that did start early is recorded as a completed result of the message, so a later park for approval does not run it twice (FR-APPR-8). The capability is off by default, so default behaviour is unchanged.

**Where it lives:** `norte-core/src/engine.rs` (`drain_stream`, `into_result`), `norte-core/src/policy.rs`, `norte-core/src/stream.rs` (`grow_tool_call`).

**FR-STREAM-5.** Observers and stream transforms see every event in order and may not drop events that affect durability; they may only annotate, filter for logging, or produce additional side-channel events.

**FR-LP-2.** Each token's probability arrives as an informational stream event tagged with the part index it belongs to: the chosen token, its raw bytes when the vendor reports them, the runner-up tokens, and any vendor field the neutral vocabulary does not model. The accumulator drops the event, so the committed message's parts and the reconstructed prefix are unchanged — asking for probabilities never moves a cache boundary. What records the request is the request digest in the message origin and the stored per-turn options (FR-EXP-7); the library persists no probabilities, and the host decides what to keep from the observer channel.

**Where it lives:** `norte-core/src/domain/logprob.rs`, `norte-core/src/engine.rs` (`stream_delta`), `norte-core/src/stream.rs` (`apply`).

**Verified by:** `norte-core` (`stream.rs`) and `norte-testing` (`tests/engine.rs`, `tests/scenarios.rs`, `tests/observers.rs`).

## 7. Engine, Phases & Turns

The engine is a pure state machine over the session. It never holds long-lived network connections; adapters own the transport.

**FR-ENG-1.** A session moves through well-defined phases (`Idle`, `Streaming`, `AwaitingTool`, `AwaitingApproval`, …). Unknown phases are rejected with a typed error.

**FR-ENG-2.** Each commit increments the session version. Concurrent writers detect conflicts via `expected_version` and must retry.

**Where it lives:** `norte-core/src/engine.rs` (`steer`).

**FR-ENG-3.** Steering messages may be enqueued at any time; they are drained at the next safe point (after the current assistant turn completes or is interrupted).

**FR-ENG-4.** The engine records a request digest with every assistant generation so that policy and approval decisions can be audited against the exact inputs. The digest is computed over the reconstructed request (Section 8), so it changes exactly when the wire changes.

**FR-ENG-5.** Reconstruction of the request from the graph is an explicit, ordered step that runs before the model call. It applies continuity replay, then media materialization, in a fixed order. No reconstruction step reads the tail to decide the prefix.

**FR-ENG-6.** A host may append an ordered sequence of messages to a session as durable history **without generating**, bridging a host-held message array (a loaded or imported transcript, or one built in memory) onto the graph. The append is a single version-checked commit (FR-ENG-2) that chains the messages onto the current head and leaves phase, turn, queue and grants unchanged. It is append-only (FR-GRAPH-4) and prefix-stable (FR-RECON-2), so the next generation sees the seeded prefix byte-identical. It is only valid from `Idle`: a running turn owns the head. The engine reports the append (FR-EVT-1).

**Where it lives:** `norte-core/src/engine.rs` (`steer`), `norte-core/src/event.rs`, `norte-testing/tests/history.rs`.

**FR-ENG-7 — Per-turn request options.** A host configures a single turn with typed `TurnOptions`: generation parameters, the reasoning request, the exposed tool set and vendor request extras. Options are layered from least to most specific — **engine defaults ← session defaults (restored by an import, FR-EXP-7) ← per-turn overrides** — and the merge is **per field**: a field set on the turn replaces that field only; an unset field inherits from the layer below. The effective options are exactly what the adapter receives (FR-ENG-9). A **vendor extra** is a provider-wire field with no neutral equivalent (for example Gemini's `safetySettings`, `toolConfig`, `cachedContent` or `responseModalities`); it is keyed `<wire-family>:<dotted.field>` (Section 12), merged per key, and spliced into the body by the adapter whose wire family matches. `send` and `resume` remain the shortcuts that use the inherited layers with no overrides; the fluent turn builder is the primary surface.

**Where it lives:** `norte-core/src/engine.rs` (`drop`, `options`, `resume`, `spawn_with`), `norte-core/src/model.rs` (`is_default`, `over`), `norte-gemini/src/generate/mod.rs` (`count_tokens`), `norte-testing/tests/turn_options.rs`.

**FR-ENG-8 — Per-turn tool subset.** A turn may expose only a subset of the registered tools to the model, addressed by `ToolName`. An unknown name is a typed `ToolNotFound` error, never a silent drop. The emitted specs always follow the registry's canonical (name) order regardless of the order the host named them, so the wire stays deterministic (FR-RECON-1); duplicates collapse. `None` means every registered tool. The engine still executes only registered tools; the subset controls what the model may *call*, not what the engine may *run*. The exposed set is part of the request identity (FR-CACHE-5).

**Where it lives:** `examples/cookbook/examples/branching.rs` (`capabilities`), `norte-core/src/engine.rs` (`default_turn_options`, `drop`, `reasoning`, `resume`), `norte-core/src/model.rs` (`over`), `norte-testing/tests/turn_options.rs`.

**FR-ENG-9 — Auditable effective request.** Every reconstructed request reports its effective request configuration — generation parameters, reasoning request and the ordered exposed tool names — together with its digest and cache boundaries (FR-EVT-4). A host or observer can therefore inspect, per send, exactly which parameters were used and which tools were enabled without re-deriving the layer merge.

**Where it lives:** `norte-core/src/engine.rs` (`generate`), `norte-core/src/event.rs`, `norte-core/src/model.rs` (`over`), `norte-testing/tests/turn_options.rs`.

**Verified by:** `norte-testing` (`tests/engine.rs`, `tests/history.rs`, `tests/turn_options.rs`, `tests/scenarios.rs`).

## 8. History Reconstruction & Prompt-Cache Stability

Providers cache the request prefix and bill cached input tokens at a fraction of fresh input tokens, while a changed prefix re-bills the whole prefix from the first divergence. Cache is therefore money. Reconstruction — the step that turns the graph into the message sequence the model sees — must be a deterministic, append-only function of the graph, so the provider’s cache keeps hitting across turns and across branches.

### 8.1 Reconstruction Rules

**FR-RECON-1 — Determinism.** Reconstruction of the message sequence is a pure function of `(session head, model reference, capability descriptor, materialization policy, continuity decisions)`. It must not depend on wall-clock time, locale, randomness, process identity, request ids or any state that changes between turns. Given the same inputs it yields a byte-equivalent sequence.

**Where it lives:** `examples/cookbook/examples/cache.rs`, `norte-core/src/domain/conversation.rs`, `norte-testing/tests/properties.rs`.

**FR-RECON-2 — Prefix stability.** Appending new nodes must not alter the reconstruction of any previously committed message. Adding turn *N+1* must produce a sequence whose first *N* turns are byte-identical to the previous request. The cached prefix is preserved.

**Where it lives:** `examples/cookbook/examples/cache.rs`, `norte-core/src/domain/conversation.rs`, `norte-core/src/engine.rs` (`steer`), `norte-testing/tests/history.rs` (`seeded_history_round_trips_and_keeps_the_prefix`), `norte-testing/tests/properties.rs`.

**FR-RECON-3 — No per-request identifiers in the prefix.** Nothing inside the cache-critical prefix may embed values that change per request: signed URLs, freshly minted vendor file ids, nonces, timestamps or request ids. Any such value must either be persisted and reused, or placed only in the ephemeral tail.

**Where it lives:** `norte-core/src/media_adapter.rs` (`resolve`), `norte-http/src/media.rs` (`download`), `norte-testing/tests/media.rs` (`an_expired_provider_file_is_revived_once_and_the_prefix_stays_stable`).

**FR-RECON-4 — Materialization policy.** Every part kind has a deterministic materialization rule to the wire:
- **ToolResult / ProviderToolResult.** Rendering — including truncation, elision markers and any structured summary — is a deterministic function of the **stored** part. Truncation thresholds belong to the persisted materialization policy identity, never to the current context length or tail. A committed result renders identically on every replay.
- **Media.** `Blob` and `Inline` materialize deterministically from content (content-addressed, so identical bytes produce an identical encoding). `ProviderFile` is reused while valid; on expiry it is re-uploaded from the blob (FR-PART-4) — a single, explicit cache invalidation point — and the fresh reference is remembered by the adapter for the rest of its life, so later turns reproduce it byte-for-byte. A restarted process re-uploads once, which is the same single invalidation point; the stored node is never rewritten (FR-GRAPH-2).
- **Extension.** Round-trips byte-for-byte (FR-PART-1).

**Where it lives:** `norte-core/src/engine.rs` (`default`), `norte-core/src/media_adapter.rs` (`ingest`, `resolve`), `norte-http/src/media.rs`, `norte-testing/tests/media.rs` (`an_expired_provider_file_is_revived_once_and_the_prefix_stays_stable`).

**FR-RECON-5 — Cache-critical vs ephemeral.** The engine distinguishes cache-critical content (participates in the cached prefix) from ephemeral content (does not). Ephemeral messages — live run ids, volatile tool scratch, timestamps, UI hints — are marked by the host through the message’s typed cache class, a persisted field whose default is critical, and are materialized strictly after the cache-critical prefix rather than dropped. Granularity is the whole message: parts of one message are not classed separately, so volatile content that must not enter the prefix is sent as its own ephemeral message and message atomicity (a call and its results) is never split. Marking a message ephemeral is an explicit, persisted host decision.

**Where it lives:** `norte-testing/tests/reconstruction.rs`.

**FR-RECON-6 — Ordering.** Reconstruction emits the stable prefix first and the ephemeral tail last. Ephemeral content must never appear before, or interleaved with, cache-critical content. Cache boundaries (FR-RECON-7) are computed from the critical prefix alone, so the tail never moves one.

**Where it lives:** `norte-testing/tests/reconstruction.rs`.

**FR-RECON-7 — Cache boundaries.** Where the host requests provider-side caching, the reconstructed sequence exposes deterministic boundary positions (for example after a stable system/developer prefix and after each completed turn). Boundaries never depend on the tail and never move when a new turn is appended.

**Where it lives:** `norte-core/src/engine.rs` (`default`, `dispatch_early`, `results_in_call_order`).

**FR-RECON-8 — Branch / rewind.** When a session’s `head` moves to another branch — rewind, retry, edit — reconstruction must reuse the longest common ancestor prefix unchanged. Rewinding to an ancestor whose prefix was already sent must reproduce that prefix byte-for-byte; only the divergent suffix may differ. Branching therefore reuses the provider’s cached prefix instead of paying for it again. A head moves by committing a node whose parent is an ancestor (FR-GRAPH-1): branching adds nodes and moves the pointer, the engine exposes no separate rewind call, and the ancestor itself is never rewritten.

**Where it lives:** `examples/cookbook/examples/branching.rs`, `norte-testing/tests/reconstruction.rs` (`a_branch_from_an_ancestor_reproduces_the_shared_prefix_byte_for_byte`).

**FR-RECON-9 — Prefix changes are explicit.** The engine never inserts, reorders or drops content during reconstruction on its own. If host content must precede earlier turns, it is appended as an ordinary message or carried in host metadata, and the resulting cache invalidation is reported (FR-EVT-4). Rewriting existing nodes to change the prefix is forbidden.

**Where it lives:** `norte-core/src/event.rs`.

**FR-RECON-10 — No override.** There is no API to bypass or disable the reconstruction rules for a request. Cache stability is not opt-out; hosts influence it only through the supported, persisted knobs: media/materialization policy, cache boundaries, ephemeral annotation and the model’s capability descriptor.

**FR-RECON-11 — Verification.** Reconstruction is verifiable: reconstructing the same head twice yields the same request digest, and appending a turn leaves the digest of the prefix unchanged. Property tests cover both invariants (FR-TEST-3).

**Where it lives:** `norte-testing/tests/properties.rs`.

**FR-RECON-12 — Auditable wire projection.** Each adapter exposes the exact JSON body it would `POST` for a reconstructed request as a pure, side-effect-free function of `(model reference, continuity state, request)`, without opening a connection. The projection is the same function used by the live send path, so a host or a test can diff the bytes a request would produce and attribute any cache-relevant change (FR-CACHE-1..4, FR-RECON-2) without spending tokens.

**Where it lives:** `examples/cookbook/examples/branching.rs` (`stream`), `examples/cookbook/examples/cache.rs` (`stream`), `examples/cookbook/examples/export_import.rs` (`stream`), `norte-anthropic/src/messages/mod.rs` (`profile`), `norte-gemini/src/generate/mod.rs` (`profile`), `norte-openai/src/chat/mod.rs` (`profile`), `norte-openai/src/responses/mod.rs` (`profile`), `norte-testing/src/probe.rs` (`guard`), `norte-testing/tests/export.rs`.

### 8.2 Conversation Export & Import

A conversation is portable data. A host can export a session as a self-contained value, persist or ship it as JSON, and import it elsewhere — possibly in another process — and keep generation, tool loops and the provider cache behaving exactly as before. This is the "hand the chat over" path: the transcript is the artifact, the store is just where it currently lives.

**FR-EXP-1 — Self-contained envelope (approval ledger excluded).** Export yields a `Conversation`: the session identity, its durable phase and turn, host metadata, the pending steering queue, the ordered message history (oldest first) and, optionally, the effective request options of the last turn (FR-EXP-7). It contains no store-internal detail — node ids, session/node versions and timestamps are store bookkeeping, not conversation content (FR-GRAPH-4). The message list is the array form.

**Where it lives:** `norte-core/src/domain/conversation.rs`, `norte-core/src/engine.rs` (`append_history`), `norte-testing/tests/export.rs` (`a_parked_approval_survives_export_import`).

**FR-EXP-2 — Lossless, byte-stable round-trip.** `Conversation` serializes to canonical JSON and deserializes back with zero information loss: `from_json(to_json(c)) == c` and `to_json(from_json(s))` reproduces `s` byte-for-byte for any JSON this build wrote. Serialization is deterministic: no map, set or object key order may vary the bytes, so two equal conversations always serialize identically. Property tests cover the invariant (FR-TEST-3).

**Where it lives:** `examples/cookbook/examples/export_import.rs`, `norte-core/src/domain/conversation.rs`, `norte-core/src/engine.rs` (`append_history`, `export`), `norte-testing/tests/export.rs` (`export_import_carries_signatures_that_the_same_model_replays`), `norte-testing/tests/properties.rs`.

**FR-EXP-3 — Versioned envelope.** The envelope carries a schema version. A reader that does not understand the version rejects the envelope with a typed error rather than guessing or partially applying it.

**Where it lives:** `norte-core/src/domain/conversation.rs`.

**FR-EXP-4 — Import preserves identity so the cache key does not move.** Importing commits the exported messages verbatim as a new session under the **same** session id (no generation; §7 FR-ENG-6 in reverse). Because adapters derive session-scoped cache keys from that id (FR-CACHE-2) and reconstruction depends only on the history plus the engine’s model, capabilities, tools and generation parameters (FR-RECON-1), a conversation exported and re-imported into an identically configured engine reproduces a **byte-identical wire prefix**, including the provider cache key. Re-homing onto a fresh id is an explicit fork the caller performs by editing the envelope; the resulting cache invalidation is intentional.

**FR-EXP-5 — Import never clobbers.** Importing a conversation whose session id already exists fails with a conflict; a restore is a create, not an overwrite. Importing an unknown schema version fails with the version error (FR-EXP-3).

**FR-EXP-6 — Losslessness is testable end to end.** The guarantee is covered by a hermetic test that seeds a history, exports it, re-imports it into a fresh engine, reconstructs the same next turn and asserts the reconstructed request and the adapter’s wire projection (FR-RECON-12) are byte-identical — for every shipped wire family (Section 12).

**Where it lives:** `norte-testing/tests/export.rs`.

**FR-EXP-7 — Request-config capture.** The envelope carries an optional `TurnOptions` recording the **effective request configuration of the last request** (FR-ENG-9). Export fills it when the exporting engine knows it (it does for a session it just ran); import restores it as the session’s defaults so a later `resume`/`turn` reproduces the **complete** request — history *and* config — without the host re-deriving the layer merge (FR-ENG-7). The history prefix stays independent: even when the options are absent (`None`), the exported messages reconstruct byte-identically and only the request-level fields fall back to the importing engine’s defaults.

**Where it lives:** `norte-core/src/domain/conversation.rs`, `norte-core/src/engine.rs` (`materialize_message`, `session_turn_options`), `norte-core/src/model.rs`.

### 8.3 Provider Cache Mapping (Adapter Obligations)

**FR-CACHE-1.** The Anthropic adapter derives `cache_control` breakpoints from the declared cache boundaries (FR-RECON-7) and never invents breakpoints mid-prefix.

**FR-CACHE-2.** The OpenAI adapter relies on automatic prefix caching and supplies a `prompt_cache_key` that is the session id itself: the same for every request of a branch, and unchanged by an export and re-import under the same id (FR-EXP-4), so a branch change or a round-trip through the envelope does not silently rotate the key.

**FR-CACHE-3.** The Gemini adapter uses implicit caching by default: the deterministic cache boundaries reconstruction computes are not sent as breakpoints, but the request is still built byte-stably so the implicit cache can hit. It additionally exposes the explicit API: `create_cached_content(request, ttl)` creates a cached content and returns its name, which a later turn references through the `google.gemini:cachedContent` vendor extension (FR-ENG-7) with the cached prefix omitted.

**FR-CACHE-4.** Adapters surface cache read / cache write token counts from vendor usage into the neutral `Usage` (Section 4.5) so cache behaviour is observable and billed cost is attributable to a branch.

**FR-CACHE-5 — Tool-set stability.** Tool declarations participate in the provider prefix on every wire family; the exposed tool set is therefore part of the request identity and of the request digest. The engine never changes it implicitly. A per-turn subset (FR-ENG-8) that differs from the previous turn is an explicit, host-controlled cache-invalidation point from the tools section onward, and the effective set is reported on every request (FR-ENG-9). A turn that exposes the same set as before is prefix-stable.

**Where it lives:** `norte-testing/tests/turn_options.rs` (`a_tool_subset_change_invalidates_the_request_but_not_the_messages`).

**Verified by:** `norte-testing` (`tests/reconstruction.rs`, `tests/export.rs`, `tests/properties.rs`).

## 9. Human Approval

Approvals are first-class durable records, not ephemeral callbacks.

**FR-APPR-1.** An approval record is bound to an argument digest, carries an expiry, optional edit history, grants and a full audit trail. It survives process restarts.

**Where it lives:** `norte-tools/src/command.rs`.

**FR-APPR-2.** A tool call that requires approval blocks until a matching grant is present or the approval expires; the session phase becomes `AwaitingApproval`.

**FR-APPR-3.** Grants may be scoped (single call, tool name, session, global) and may be revoked. Revocation is itself audited. Scope is enforced, not merely recorded: a call whose id has no decision yet is still authorized by a standing grant that covers it, so a tool- or session-scoped grant lets the agent keep working without asking the operator again. `Engine::revoke` takes a grant back: it audits the revocation and drops the grant from the session's standing grants in the same commit, so a revoked grant stops authorizing both the call it was issued for and the later calls it used to cover.

**Where it lives:** `norte-core/src/engine.rs` (`authorize`, `deny`), `norte-core/src/event.rs`.

**FR-APPR-4.** Tampering with the argument digest after a grant has been issued invalidates the grant: the call fails with the reason, rather than the session parking on a record whose digest can never match again.

**Where it lives:** `norte-core/src/engine.rs` (`authorize`).

**FR-APPR-5.** The ledger is store state, not conversation content, so it does not travel in the export envelope (FR-EXP-1). A session in `AwaitingApproval` whose parked calls have no undecided record re-requests approval when it runs again: the engine re-derives the request from the head message's tool calls and the phase. This makes a parked conversation exportable, importable and resumable — the host is asked again, on the host that now holds the session — and it is also what delivers a denial, which otherwise has nothing to act on.

**Where it lives:** `norte-core/src/engine.rs` (`run`).

**FR-APPR-6.** `Store::approvals` returns every approval record recorded for a session, decided or not, in request order; the engine's `pending_approvals` narrows that to the records still awaiting a decision (`Pending` and unexpired). A host that reads the port directly and offers every record it finds will re-ask questions that were already answered.

**Where it lives:** `norte-core/src/engine.rs` (`turn_metadata`).

**FR-APPR-7.** A denial is not a dead turn. The denied call comes back to the model as a failed tool result carrying the operator's reason, exactly like a policy denial or a failing tool (FR-TOOL-3), so the agent can adapt rather than the turn aborting. The record stays in the store with its audit trail (FR-APPR-1).

**FR-APPR-8.** A call that already ran is never run twice. When a message with several tool calls parks on a later call's approval, the results already produced are recorded with the park commit under the engine-managed metadata key `norte.partial_tools` (a sibling of `norte.turn_options`, FR-EXP-7), and tool execution seeds them on resume and skips any call whose result is already present. The key is dropped when the tool message commits and when a new generation starts, so it never outlives the message it belongs to; it travels in the export envelope with the rest of the session metadata, so an exported parked session does not re-run its calls on import (FR-EXP-1). The committed tool message always carries the results in the assistant message's call order, whatever order they were produced in — which is what early execution needs, since a call started early can complete after a later one is still deferred (FR-STREAM-4).

**Where it lives:** `norte-core/src/model.rs`, `norte-testing/tests/engine.rs` (`a_park_after_a_call_does_not_run_that_call_again`).

**Verified by:** `norte-core` (`engine.rs`) and `norte-testing` (`tests/engine.rs`).

## 10. Tools

### 10.1 Tool Kinds

Three tool kinds exist in the foundation:

- **Function** — host-executed, schema-defined, typed arguments and results.
- **Provider-executed** — the vendor runs the tool (search, code interpreter, …); results return as `ProviderToolResult` parts that must be replayed via continuity tokens.
- **Provider-schema** — vendor-defined fixed schemas that the host may still intercept for approval or logging.

### 10.2 Command / Bash Executor (Flagship Tool)

The default tools suite (`norte-tools`) ships a production-grade command executor, designed to be the highest-quality shell/tool execution surface available in any agent framework. Core defines only its contract and registration.

**FR-TOOL-BASH-1.** The executor accepts a structured request: program (or shell) and arguments, working directory, environment additions, timeout, maximum output bytes, and stdin (inline or a blob). Every bound in the request is a *request*: the timeout and the output cap are clamped to the host's ceilings (FR-TOOL-BASH-3's model budget included), and the environment additions cannot replace a variable the host set.

**FR-TOOL-BASH-2.** Output is streamed back to the agent as it is produced — stdout and stderr separately, or merged into one stream whose bytes appear in the order the readers saw them. Both are bounded: the live stream carries at most the same bytes per stream as the capture keeps, and excess output is truncated with a clear marker and a typed `Truncated` flag. The truncation rule is deterministic and persisted (FR-RECON-4), and the byte count reported is the whole of what the process wrote, not what was kept.

**FR-TOOL-BASH-3.** Context returned to the model is deliberately rich and structured: exit status, duration, bytes read, truncation status, working directory at exit, and an optional machine-readable summary of the most recent N lines or of matched patterns. Two bounds keep it honest: the text that reaches the model is capped by the host, elided around both ends with a marker saying how much, and the summary's tail is taken from the last output the process wrote rather than from the end of the capped head. The executor's own failures travel in a field of their own, so the process's stderr is never confused with the executor's diagnostics. The host may supply a custom context renderer.

**FR-TOOL-BASH-4.** Feedback to the model distinguishes clean success, non-zero exit, death by a signal the executor did not send, timeout, cancellation, an unusable request, spawn failure and infrastructure failure. Each case carries a stable error code and human-readable detail so the model can decide whether to retry, amend the command, or abandon the approach; a broken pipe or a wait that failed is reported as an infrastructure failure instead of passing as success.

**FR-TOOL-BASH-5.** The process group does not outlive the call. On timeout, on cancellation, when a pipe is still held past the drain deadline and when the call's future is dropped, the group is signalled, a grace period is observed and a forced kill follows, so a process that ignores the polite signal still dies and its children go with it. Partial output already produced remains available. Because a command can hand its pipes to a background child and exit, the wait for output after the process is reaped is itself bounded: a child that holds them cannot stall the call, and the result says the pipes outlived the drain.

**FR-TOOL-BASH-6.** The executor never runs with ambient credentials. The process starts from the host's environment baseline — empty unless the host supplies one — plus the request's additions, and a variable the host set wins. The secrets the host declares are redacted before anything is kept or streamed: in the model's text, in the live stream and in the audit record, and the request can neither switch redaction off nor choose what counts as a secret. Redaction holds the tail of what it has read until no later byte can complete a secret, so a read boundary, the truncation cap or a merge cannot split one into visibility.

**FR-TOOL-BASH-7.** The executor does not authorize. A request can name any program, so a sandbox profile, a capability flag or an allowlist that the *request* selects would be theatre: deciding what may run belongs to the host, through the approval ledger and policies (FR-APPR-1, FR-POL-1). The executor's contribution is containment and boundedness — the group dies with the call, the output is capped, the environment is the host's — and a host that wants a filesystem or network sandbox applies one outside the tool, where the request cannot reach it.

**FR-TOOL-BASH-8.** Every invocation records a structured audit event containing the redacted command line, the working directory, the request digest, start/end timestamps, exit status or signal, byte counts, truncation, the reason it failed, and the hash of the text the result carried — enabling later reconstruction without re-execution. The digest is taken over the request as the executor judged it, with any declared secret already redacted, so it can neither leak a secret nor be brute-forced into yielding one.

### 10.3 Tool Registration & Schema

**FR-TOOL-1.** Tools are registered with a unique name, JSON Schema (or a typed Rust schema that generates it), a human description, and an optional approval policy reference.

**Where it lives:** `norte-core/src/engine.rs` (`into_result`).

**FR-TOOL-2.** Argument parsing is strict: unknown fields, type mismatches and constraint violations become typed errors before the tool body runs.

**Where it lives:** `norte-tools/src/edit.rs` (`unknown_arguments_are_rejected_before_the_body_runs`), `norte-tools/src/read.rs` (`unknown_arguments_are_rejected_before_the_body_runs`), `norte-tools/src/write.rs` (`unknown_arguments_are_rejected_before_the_body_runs`).

**FR-TOOL-3.** Tool results may contain text, media parts and optional structured JSON; the engine packages them into a single `Tool` message for the next model turn.

**FR-TOOL-4.** A tool may declare its name and description with an attribute macro applied to the type that implements the executor. Registration then resolves the argument and result types from that executor implementation, so the declaration never restates them.

**FR-TOOL-5.** A tool whose executor returns the rich output envelope (text parts, media, an error flag) is registered through its own declarator, so the envelope reaches the model without being serialized a second time.

**Where it lives:** `norte-core/src/tool/mod.rs` (`from`).

**FR-TOOL-6.** The argument schema the adapters put on the wire is self-contained: before the declaration leaves the engine, local references are expanded into place and the definition block is dropped. A model that does not resolve `$ref` therefore still sees the whole shape and stops guessing at it — the failure mode is sending a string where an object is required. Both spellings of the block are honoured, `$defs` and draft-07's `definitions`, so a schema that arrives from an MCP server is expanded as well. A reference that cannot be expanded (an unknown name, or a recursive type, where expansion would not terminate) is left intact together with its definitions, so the document stays valid either way. Provider-supplied declarations are re-emitted verbatim and are not rewritten. The prose of a declaration answers to two audiences and the two are kept apart: the `///` comments in an executor crate are rustdoc for whoever maintains the executor, while `#[schemars(description = ..)]` and `#[schemars(example = ..)]` author the text the model reads (a tool's own description is already a model-facing literal in its declaration). An internal requirement code or an unresolved rustdoc link therefore never reaches the wire, and a nested shape the field name does not describe — a line range, a search-and-replace pair — is shown with an example instead of being left to guessing. Where serde carries the shape with a tag, the tag is written into the schema as the variant's discriminator: an internally tagged enum (`#[serde(tag = ..)]`) also derives `#[schemars(tag = ..)]`, so each variant names its tag with a `const` and a model that never reads the example still learns the key instead of spending a request on it — the shape that separates a program invocation from a shell script is the one seen live.

**Where it lives:** `examples/cookbook/examples/tools.rs`, `norte-tools/tests/wire_schema.rs`.

### 10.4 File Executors

`norte-tools` ships one executor for each verb a coding agent needs over a text file: `read` to look, `write` to create or replace whole, `edit` to change part. All three share the same bounds, the same failure shape and the same audit trail, and none of them decides *what* may be read or written: the path comes from the request, and the host's policy decides whether it is allowed (FR-POL-1). They are the tools the default agent runs unattended, so every requirement below is about staying honest when the file system, the model or the arguments misbehave. Each request type is `#[non_exhaustive]` with a typed builder, and the same type deserializes the JSON a model produces, so one shape serves both callers and a field added later breaks neither.

None of them confines a path, and none canonicalizes one. *What* may be read or written is not a property of the string in a request: a path can be a symbolic link, a relative name, or a name that a later traversal reaches through a directory the host never meant to expose, and a check this crate makes by walking the name itself is a guess about the file system at the instant of the check. That decision belongs to the OS sandbox the host runs the executor in — seccomp, a container, a bind mount, a dedicated user (FR-POL-1, FR-TOOL-BASH-7) — which is the layer that can make it true. The mechanism to do it in-process also exists (`openat2(RESOLVE_BENEATH)` against a directory handle resolves a whole path beneath a root); the decision is that this executor does not implement it, because the guarantee would be conditional on the platform and on the host's handles whereas the sandbox's is not.

**FR-TOOL-FILE-1 — Every file executor is bounded.** A read takes its size limit from the bytes actually read, not from metadata, so a file that grows between a check and the read cannot slip past it; a write is refused before I/O when its content is over the configured maximum, stages the new content in a sibling temporary file, creates missing parents, keeps the permissions of a file it replaces and renames into place, so a reader sees either the old file or the new one and an interrupted call never leaves a half-written file. The temporary is opened with `create_new`, so it is never written through a name that is already there — a symbolic link planted at a guessed name would otherwise have the write truncate whatever it points at — and its `.norte-tmp` name carries a random part beside the process id, so guessing it in advance takes more than reading the process id; a collision is retried a bounded number of times and then reported as an error naming the file. The temporary is removed on every failure this crate can see; a process killed between the staging and the rename is the one case where it can outlive the call, and the suffix is what makes the leftover identifiable.

**FR-TOOL-FILE-2 — Line terminators are preserved.** The line at the edit site decides, and nothing else does: the replacement's own endings are rewritten to the terminator of that line, so an edit into a CRLF line ends its lines with CRLF and an edit into an LF line ends them with LF, and a file gains no mixed endings from an edit. One stray CRLF elsewhere in the file therefore no longer makes every inserted line CRLF. When that line has no terminator of its own — the last line, unterminated — the rule falls back to the terminator most of the file's lines use. The two paths differ in exactly one observable respect, and it is stated rather than smoothed over: the byte-exact path replaces the bytes the block matched, terminators the block carried included, while the line-based path replaces whole lines and keeps the terminator the replaced lines ended with, so the line following the edit stays whole.

**FR-TOOL-FILE-3 — Failures name the file.** Every I/O failure reaches the model with the path it concerns, because `std::io::Error` alone does not carry one and the path is what tells a wrong argument from a wrong file; an oversized file is a dedicated `LimitExceeded` carrying the limit.

**FR-TOOL-FILE-4 — A write lands on what the name points at.** When the last component of the path is a symbolic link, the link is followed and what it points at is what is written, so the link survives: a rename onto the name would otherwise replace a link with a copy of the file, which is what a dotfile manager, a linked configuration or a checkout of symlinked sources would lose. Only that component is resolved — how the directory was reached is still the host's business (§10.4) — the chain is followed at most a bounded number of hops, and a chain that never ends is refused with the path named. A link whose target does not exist yet is followed too: creating through a dangling link creates its target.

**FR-TOOL-WRITE-1 — Whole-file creation and replacement.** The write executor takes a path, the complete content and an `overwrite` flag. With `overwrite` off, the file is created by hard-linking the staged temporary onto the path: the *creation* is the check, because a writer that looks first and then renames lets the loser of a race silently replace the winner. An existing file makes the call report `existed: true` with nothing written; a filesystem where a hard link cannot be made gives a typed error naming that limitation instead of falling back to the racy sequence; with `overwrite` on, the file is replaced. The result reports whether the file existed, whether the file was created, whether parent directories had to be created, and the byte and line counts of the result.

**FR-TOOL-WRITE-2 — Every write is auditable.** Each call returns the digest of the request and, when the file changed, the digest of what was written, alongside the same facts a structured audit event records, so a later reconstruction can tell what a call did without running it again (FR-TOOL-BASH-8).

**FR-TOOL-EDIT-1 — Search and replace, all or nothing.** The edit executor applies an ordered list of search/replace pairs to an in-memory copy and writes the file once: either every pair applies or nothing is written and the file stays byte-identical. Content that comes out byte-identical to what was read is reported as `unchanged` and does not rewrite the file, so an edit that changes nothing does not move a modification time a watcher is watching. Immediately before the write the file is read again and its digest compared with the one the edit was prepared from; a version that is no longer there is refused with a typed error instead of being silently overwritten. That narrows the read-modify-write window rather than closing it — two writers can pass the check at the same moment, and only a lock or a compare-and-swap closes it — so what it buys is detection, and the model is told to read the file again, never that the edit landed.

**FR-TOOL-EDIT-2 — Arguments are checked before the file is touched.** A missing path, an empty list of edits, an edit whose search block is empty, a search or replacement block larger than the configured maximum, or a request with more edits than the configured maximum all fail with a typed `InvalidInput` before any I/O, so a request that could not succeed never reads and never writes a file. A search of nothing but whitespace is not an empty search: it is a block, and uniqueness is what decides whether it may apply.

**FR-TOOL-EDIT-3 — A whitespace-only difference still applies.** When a search block matches nothing exactly, whole lines are compared with whitespace runs collapsed, so a block off by indentation alone still lands, and the match is reported as whitespace rather than as exact. That is the whole of the default tolerance: a block that differs in content is refused, and a host that wants more sets a similarity threshold explicitly — with a margin the runner-up has to clear, because two windows that close are a choice this executor has no grounds to make. The threshold, the block size, the edit count and the comparison budget are configurable with documented defaults; a threshold outside `0.0 ..= 1.0` is read as `1.0`, and a value that is not a number leaves only exact matches. The replacement's indentation moves from the block's base onto the file's, level by level, so a block written in four-space steps lands in a tab-indented file as tabs, while a line whose indentation does not begin with the block's is left exactly as the model wrote it — the executor translates the indentation the block lets it translate and invents nothing. Every comparison is charged to a budget, so a block that belongs nowhere is refused in bounded time instead of holding the runtime.

**FR-TOOL-EDIT-4 — Ambiguity is refused, with the evidence.** A block that matches in more than one place, or in several equally close ones, applies nowhere and comes back carrying the similarity it reached, the number of places that matched, and the lines there — bounded in number, distinct, clipped in width, with the characters a terminal would act on escaped. Two hits that overlap are two places: the scan steps one character past each match, so `aa` in `aaa` is not a unique match. More places than the executor scores is ambiguous, not a miss, and the count then reported means "that many or more". The lines shown come from the copy the edit was applied to — the file itself when the first edit failed, and the file as the earlier edits of the same call would have left it otherwise — and the result says which it is, together with the statement that nothing was written, so the next attempt can be exact (FR-TOOL-EDIT-3).

**FR-TOOL-EDIT-5 — A refusal is a result, not a broken call.** A miss or an ambiguity is a normal tool result flagged as an error, carrying the evidence and the statement that nothing was written; the model reads it, changes the block and tries again instead of treating the call as infrastructure failure. Whatever the outcome, the audit event carries the digest of the content the call worked from and the digest of what it left — so a host has the version an edit replaced — and, since the fallback is the one thing here the model did not ask for, how many of the matches applied without an exact one and how close the loosest of them was.

**FR-TOOL-READ-1 — A read is a projection, not the file.** The read executor shows the lines a request selects and reports what the file holds: its size, its line count, how many lines the request selected, how many were shown, whether part of the selection was left out, and the line to continue at. A file larger than one result is therefore paged rather than refused, and a model that only needs one fact never pays for the whole file. A file larger than the configured maximum is the one thing that is refused rather than paged, with `LimitExceeded` (FR-TOOL-FILE-3).

**FR-TOOL-READ-2 — Text is classified; binary is refused.** Only the bytes are trusted: content that opens with a byte-order mark of a foreign encoding (UTF-16 or UTF-32) or holds a NUL byte is binary — a text file of one of those encodings never contains a NUL byte outside ASCII, so the mark is what gives it away — and fails the call instead of being sprayed into the context. Content that is valid UTF-8 is text; anything else is shown with replacement characters and an explicit note, so the model knows the bytes it reads are not the bytes on disk. The classification is reported with every result, so a host reading the structured form knows which of the three it got.

**FR-TOOL-READ-3 — Arguments are checked before the file is touched.** An empty path, a zero-based range start, a range that ends before it starts, a tail of zero, a tail combined with a range or a pattern, and any argument that only means something with a pattern (`invert`, `ignore_case`, `count_only`, `context`, `max_matches`) used without one, are typed `InvalidInput` failures before any I/O. A pattern that does not compile fails the call naming the pattern.

**FR-TOOL-READ-4 — One result is bounded, and paging always terminates.** The file size, the bytes of one result, the lines one result shows, the lines a pattern selects and the width of one line are all configurable with documented defaults, and a limit set to zero is raised to one, so every result shows at least the next line: `next_start` is always strictly beyond the line the read began at, and repeating the read with it reaches the end of the file without repeating or skipping a line. What a pattern may cost is bounded before it runs as well: a pattern longer than the configured maximum or containing a line break is refused, the compiled program is given memory limits, the capture groups one line may report are capped — a pattern that declares more is refused rather than silently truncated — and the groups reported are charged to the same byte budget as the lines, so no pattern can make one result unbounded.

**FR-TOOL-READ-5 — Selection is exact and reported.** A read takes a range, a tail, or the lines a pattern selects, optionally with context around each, inverted, case-insensitive, or counted without being shown. The result marks selected lines apart from context lines, carries the capture groups and the 1-based character column of the first match on a line, and is rendered in the line-number convention a developer already reads (`5:` for a selected line, `5-` for context, `--` between blocks). A line wider than the configured width is clipped — around the match when a pattern selected it, so what the pattern found stays visible, and from the end otherwise — and a trailing `…` is the documented marker that the width cut it; a line that genuinely ends in `…` can therefore look clipped, which is the one ambiguity in the rendering. Characters a terminal would act on (C0 controls and bidirectional overrides, a tab excepted) are escaped rather than sent raw, so a line in a file cannot clear the screen, rewrite what came before it or reorder the report.

**Verified by:** `norte-tools` suite (`command.rs`, `read.rs`, `write.rs`, `edit.rs`).

## 11. Layers, Policies & Observers

The interceptor chain is replaced by three orthogonal, fully typed mechanisms. They are composable, order-explicit, cancellation-aware and leave a reproducible record.

### 11.1 Layers

A layer is a typed middleware that wraps the entire request/stream lifecycle. Layers form an explicit stack (outermost first). Each layer receives a continuation and may inspect or transform the request, the stream, or the final outcome.

**FR-LAYER-1.** Layers are pure with respect to the store: any durable side-effect must go through the engine’s commit path.

**FR-LAYER-2.** A layer may short-circuit (return a synthetic stream) or forward to the inner continuation. Short-circuit reasons are typed and appear in the audit trail. A layer must not alter the cache-critical prefix (FR-RECON-2).

**Where it lives:** `norte-core/src/layer.rs` (`short_circuit`).

**FR-LAYER-3.** The stack is built once at agent construction; runtime mutation of the stack is forbidden, eliminating ordering races.

### 11.2 Policies

Policies are pure functions from a typed view of the current request (messages, tool calls, model capabilities, session metadata) to a decision. Decisions are stored with the policy’s identity so that the same inputs always produce the same recorded outcome.

**FR-POL-1.** Built-in decision kinds include `Allow`, `Deny(reason)`, `RequireApproval`, `Transform(arguments)`, and `RateLimit`.

**Where it lives:** `examples/cookbook/examples/agent.rs`, `norte-tools/src/command.rs`, `norte-tools/src/edit.rs` (`default`), `norte-tools/src/fs.rs` (`io_error`), `norte-tools/src/read.rs` (`default`), `norte-tools/src/write.rs` (`default`).

A policy decides what may be *attempted*; whether an attempt is *safe* is the sandbox's. The host tools do not confine paths and never canonicalize them (§10.4): a `Deny` is enforced by the OS sandbox the executor runs in — seccomp, a container, a bind mount, a dedicated user — which is the layer that holds even for a path no string-matching policy could have predicted (FR-TOOL-BASH-7). The file executors resolve a symbolic link at the last component so a write lands on what the name points at (FR-TOOL-FILE-4) and leave everything else — `..`, traversal, hard links — exactly as the request wrote it.

**FR-POL-2.** Policies are evaluated in a fixed, declared order. Later policies see the decisions of earlier ones.

**Where it lives:** `benchmarks/agent-cost/src/main.rs` (`turn_options`), `examples/cookbook/examples/agent.rs` (`answer`).

**FR-POL-3.** A policy must not perform I/O; any external lookup belongs in a layer that feeds data into the policy view.

**FR-POL-4.** The view carries two views of the request in flight: `history`, the messages of the request, and `results`, the tool results the same assistant message has already produced, in call order. `results` is a sequential concept and its contents depend on the execution path: a call started early (`EngineConfig::early_tool_execution`, FR-STREAM-4) is evaluated before anything has run and sees an empty `results`, and a call deferred to the normal path is evaluated as the message's calls complete, so it sees every result produced so far — including those of calls that follow it in the message. A host whose policies depend on `results` must therefore leave early execution off, which is the default; the engine does not infer the dependency.

**Where it lives:** `norte-core/src/engine.rs` (`into_result`, `partial_results`, `request_messages`, `run_tools`), `norte-core/src/policy.rs`.

### 11.3 Observers & Stream Transforms

**FR-OBS-1.** Observers receive a read-only view of every engine event (stream deltas, phase changes, commits, approvals). They are fire-and-forget with respect to the critical path; a slow observer must not block the stream.

**Where it lives:** `norte-testing/tests/observers.rs`.

**FR-OBS-2.** Stream transforms may annotate or project events for downstream consumers (UI, metrics) but may not drop or reorder events that the engine relies on for durability.

**Where it lives:** `norte-core/src/engine.rs` (`commit`, `observers`), `norte-core/src/observer.rs` (`transform_names`).

**FR-OBS-3.** Both observers and transforms are registered with a unique name and appear in the session’s extension metadata so that runs remain reproducible.

**Where it lives:** `norte-core/src/engine.rs` (`observers`), `norte-core/src/observer.rs` (`names`), `norte-testing/tests/observers.rs` (`a_transform_projects_the_deltas_without_touching_the_commit`).

### 11.4 Design Rationale

The three mechanisms separate concerns that a single interceptor chain conflated: request/response wrapping (layers), pure decision logic that must be auditable (policies), and non-critical observation (observers). Integer priorities and mutable global chains are eliminated. Everything that affects control flow is typed, ordered and recorded.

**Verified by:** `norte-core` (`observer.rs`, `policy.rs`) and `norte-testing` (`tests/observers.rs`).

## 12. Provider Adapters

Adapters must expose the documented public surface of their respective providers for the features advertised by the capability descriptor. Gaps are compile-time or runtime typed errors, never silent omissions. Two things keep that honest: a feature the library does not implement is absent by an explicit decision and never half-works, and an opt-in vendor field the request never asked for is not part of the advertised surface — the adapter does not invent a neutral home for data nobody requested. Log probabilities are requested explicitly, per turn (§5, §6), and surface as an informational event the host may keep or drop; because the request asked for them, they are no longer an unrequested vendor field.

A host-executed function tool is declared by its JSON-schema `parameters`. A tool the provider itself executes or defines (a built-in such as web search, computer use, code execution or URL context) is declared by the `ToolSpec::declaration` field, which holds the provider’s declaration object and is re-emitted verbatim on the wire: OpenAI Responses and Anthropic place it as an entry in the same `tools` array, Gemini places it as a separate entry alongside `functionDeclarations`, and Chat Completions, which cannot express it, returns a typed capability error. A provider tool whose `declaration` is absent is a typed capability error on every wire.

Request fields with no neutral equivalent are supplied as **vendor extras** on `TurnOptions` (FR-ENG-7), keyed `<wire-family>:<dotted.field>` — for example `google.gemini:safetySettings` or `google.gemini:generationConfig.responseModalities`. The adapter whose wire family matches splices the field into its request body (creating intermediate objects for a dotted path); a key targeting another wire is ignored, and a path that collides with a field the adapter already set is a typed capability error, so a host can never silently override adapter-owned wire state.

Input media map onto each wire as follows; a kind a wire cannot express is a typed error, so a modality a descriptor advertises but the wire cannot carry is never sent silently.

| Media kind | Gemini | Anthropic | OpenAI Responses | OpenAI Chat |
| --- | --- | --- | --- | --- |
| Image | `inlineData` / `fileData` | `image` | `input_image` | `image_url` |
| Document | `inlineData` / `fileData` | `document` | `input_file` | `file` |
| Audio | `inlineData` / `fileData` | — typed error | `input_file` | `input_audio` (inline `wav` / `mp3`) |
| Video | `inlineData` / `fileData` | — typed error | `input_file` | — typed error |

Model-generated media streams back through the same events: audio output arrives as `MediaStart` / `MediaChunk` with its transcript and (on Chat) the vendor audio id settled by a `MediaMetadata` event, and image-generation output as an image media part. An assistant audio turn is replayed by reference (the vendor id), never by re-sending the bytes as input.

### 12.1 OpenAI

**FR-OAI-1.** Responses API is the primary wire format. It carries reasoning items, built-in tools and supports stateless replay. Streamed text, reasoning, refusal (deltas settled by `response.refusal.done`), audio output (`response.audio.delta` / `response.audio.done`, materialized as an `Audio` media part streamed as `audio/pcm16` with its transcript settled into the part metadata), image-generation output and URL/file citations are mapped. Token accounting maps cache read/write and audio input/output tokens, keeps the counters with no neutral field (prompt/completion text and image tokens, accepted and rejected prediction tokens) in `Usage::extension`, and keeps the rest of the vendor usage object — the price, the flags, the vendor's own cost breakdown — verbatim in `Usage::vendor` (FR-ADP-5). Every other documented output item — the server-side tool calls and results such as web search, file search, code interpreter, computer use, MCP, shell and apply-patch — round-trips verbatim as an `Extension` part (FR-ADP-3), so a documented item is never dropped.

**FR-OAI-2.** Chat Completions is supported only for the compatible long tail and for vendors that implement the same wire format; quirk flags document known deviations. The host declares a vendor's deviations as `Quirk`s on the `VendorProfile` (`no_stream_usage`, `no_structured_output`, `no_stop_sequences`, `no_seed`, `no_reasoning_effort`, `no_prompt_cache_key`); each flag omits the corresponding optional field, and a structured-output request against a vendor declared `no_structured_output` is a typed capability error. Deviations are therefore explicit host inputs, never runtime discoveries. Streamed refusal, annotations, reasoning, streamed audio output (`delta.audio`, materialized as an `Audio` media part with its transcript and vendor id preserved, replayed as the assistant message's `audio` field) and the deprecated `function_call` shape (name and arguments without an id, mapped onto a tool call) are decoded rather than dropped, and an assistant `Refusal` part is re-emitted as the message's `refusal` field.

**Where it lives:** `norte-http/src/quirk.rs`, `norte-http/src/vendor.rs` (`with_header`), `norte-openai/src/chat/request.rs`.

**FR-OAI-3.** All documented tool types, modalities and reasoning controls are mapped into the neutral model. A documented output item that has no typed neutral form is preserved as an `Extension` part (FR-ADP-3), so no documented item is dropped.

### 12.2 Anthropic

**FR-ANT-1.** The Messages API is fully mapped, including thinking blocks, citations, computer-use / bash tool results, and all content block types.

**FR-ANT-2.** Thinking signatures are carried exclusively as continuity tokens; core never inspects their payload.

### 12.3 Gemini

**FR-GEM-1.** generateContent is fully mapped, including thought signatures required for function calling on models that demand them.

**FR-GEM-2.** Role mapping follows the adapter’s documented policy; `System` and `Developer` messages are placed according to Gemini’s current instruction semantics.

### 12.4 Common Adapter Obligations

**FR-ADP-1.** Every adapter implements the `Model` trait, accepts a host-supplied capability descriptor, and supplies continuity token build / merge / replay methods.

**FR-ADP-2.** Wire role names are never leaked into the neutral graph; semantic roles are mapped at the boundary.

**FR-ADP-3.** Unknown wire blocks become `Extension` parts and are returned unchanged on the next request to the same vendor, in any message role; a host never sees an unknown block dropped silently.

**Where it lives:** `norte-anthropic/src/messages/stream.rs` (`block_stop`), `norte-gemini/src/generate/stream.rs` (`code_execution_result`, `part`, `uri_only`), `norte-openai/src/responses/stream.rs` (`output_item_done`).

**FR-ADP-4.** Adapters implement media materialization (FR-RECON-4) and, where a vendor returns generated media as a URL or file, its ingestion into `BlobStore` (FR-PART-5).

**FR-ADP-5.** A vendor usage object is read through a claim: the adapter takes the counters it models out of a copy of the payload, and every field no claim touched is kept verbatim in `Usage::vendor`. A counter the neutral vocabulary has no field for is therefore never dropped, and an adapter that stops claiming one duplicates it visibly in `vendor` instead of losing it silently. The map serializes only when non-empty, so a payload whose every field was claimed adds no bytes, and an Anthropic usage record spread over several events is merged before it is read once, so no field is claimed twice. Being key-ordered and made of `Eq` values, it keeps an export byte-identical across a round trip (FR-EXP-2).

**FR-LP-3.** Each wire maps the neutral count onto its own fields and parses what it returns. OpenAI Chat sets `logprobs` and, above zero, `top_logprobs`, and reads `choices[].logprobs.content[]` and `.refusal[]`. OpenAI Responses sets `top_logprobs` and adds `message.output_text.logprobs` to `include`, then reads `response.output_text.delta.logprobs`. Gemini sets `generationConfig.responseLogprobs` and, above zero, `generationConfig.logprobs`, and reads `candidates[].logprobsResult`, pairing each chosen token with its top candidates. A wire whose model does not declare the feature is rejected by the capability gate (FR-LP-1) before the request is built, never silently ignored; a vendor entry with no token or no probability is skipped rather than guessed at; and a field the neutral vocabulary does not model (a Gemini `tokenId`, a vendor extra) travels in the entry's extension map.

**Where it lives:** `norte-openai/src/chat/request.rs`, `norte-openai/src/chat/stream.rs`, `norte-openai/src/responses/request.rs`, `norte-openai/src/responses/stream.rs`, `norte-gemini/src/generate/request.rs`, `norte-gemini/src/generate/stream.rs`.

**Verified by:** Adapter fixture suites; CI jobs **Live Provider Smoke (FR-TEST-2)** and **Parser Fuzzing (FR-TEST-3)**.

## 14. Storage Contracts

**FR-STORE-1.** `Store` and `BlobStore` are traits in core. Core also ships `InMemoryStore` and `InMemoryBlobStore` (`norte_core::memory`), which satisfy the full contract — atomic commits, optimistic concurrency, the approval ledger, content-addressed bytes — and are ready to use with no external service. A host that needs durability implements the two traits over its own database and object storage.

**FR-STORE-2.** A `Store` must support atomic commits with optimistic concurrency (`expected_version`), loading a session by id, walking the ancestor chain of a node, and enumerating children for branching UIs.

**FR-STORE-3.** `BlobStore` is content-addressed. Puts are idempotent; a `BlobId` is the hash of the bytes. Content addressing is what makes media materialization deterministic (FR-RECON-4).

**FR-STORE-4.** No concrete database driver or object-storage client appears in core or in the adapter crates.

**Verified by:** `norte-core::memory` and the conformance suites in `norte-testing` (`store_conformance`, `model_conformance`).

## 15. Events

The engine emits a typed event stream that observers and host code may subscribe to. Events are the sole notification channel; there is no parallel callback registry.

**FR-EVT-1.** Event kinds include: `SessionCreated`, `PhaseChanged`, `StreamDelta`, `PartCompleted`, `ToolCallStarted`, `ToolCallFinished`, `ApprovalRequested`, `ApprovalGranted`, `ApprovalRevoked`, `HistoryAppended`, `CommitSucceeded`, `CommitConflict`, `LayerShortCircuited`, `RequestReconstructed`, `PrefixInvalidated`, `Error`, and extension events namespaced by the registering layer or observer. The kinds are not a promise that every part arrives as one: `PartCompleted` fires for a part the wire closes explicitly — a tool call whose block carries its own completion event, a provider tool call or result, a whole `Part`, the closing media chunk — while text and reasoning parts are held in the accumulator and handed back when the stream ends, and a tool call whose wire signals no completion ends the same way. A tool *result* is not a stream event at all: it is produced after the assistant message commits and reaches a host through the store, which is where a renderer reads it to show the bytes the model is about to be handed (FR-TOOL-3, FR-STREAM-4).

**Where it lives:** `norte-testing/tests/history.rs` (`seeded_history_round_trips_and_keeps_the_prefix`).

**FR-EVT-2.** Events that carry sensitive material (continuity token payloads, raw secrets) are redacted by default; only length or presence is reported.

**FR-EVT-3.** The event stream is bounded and lossy for non-critical observers under back-pressure; critical durability events are always delivered to the store before the corresponding event is emitted.

**FR-EVT-4.** The engine reports cache-relevant facts — request digest, cache boundary positions, the effective request configuration (generation parameters, reasoning request and ordered exposed tool names; FR-ENG-9) and any prefix invalidation caused by a host action — so cache cost is observable and attributable per send.

**Where it lives:** `norte-core/src/engine.rs` (`preprocess_history`), `norte-core/src/event.rs`.

**Verified by:** `norte-core` (`event.rs`, `stream.rs`) and `norte-testing` (`tests/observers.rs`).

## 16. Error Model

**FR-ERR-1.** All public errors are typed, non-exhaustive enums. They implement `std::error::Error` and carry enough context for diagnostics without panicking.

**FR-ERR-2.** Provider wire errors are mapped into a common taxonomy (`RateLimited`, `InvalidRequest`, `Authentication`, `ContentFilter`, `ModelOverloaded`, …) while preserving the original vendor code and message in an extension field.

**FR-ERR-3.** Capability mismatches, continuity blocks, approval expiry, a grant that no record holds, store conflicts and unrecoverable media (a source whose blob is gone) each have dedicated error variants.

**FR-ERR-4.** What the transport does by itself is an explicit, configurable policy (`TransportPolicy`) rather than a hard-coded habit: how many times a connection that never reached the provider may be retried, with exponential backoff and a cap, and three timeouts — establishing a connection, a whole non-streaming request, and the silence between two events of a stream (a stalled answer fails the turn instead of hanging it forever). The transport never retries once the provider has answered, nor once a stream has started delivering: a generation must not run twice. Everything the provider itself reported — a rate limit, an exhausted quota, a `5xx` — reaches the host unchanged and with the vendor's `Retry-After` attached, plus `is_transient` to say which kinds may clear on their own, because waiting, switching model or telling the user is host policy, not transport policy.

**FR-ERR-5.** A turn that fails is never lost and never duplicated: the user message stays committed, any tool results already produced stay committed (FR-APPR-8), and the session stays in `Streaming`, which is exactly where `resume` picks it up. A host that retries a transient failure therefore re-issues the turn without the agent having to ask again.

**Where it lives:** `norte-testing/tests/export.rs` (`a_model_switch_mid_tool_loop_is_blocked_instead_of_dropping_the_signature`).

**Verified by:** The workspace suites assert the typed variants; no external input reaches an unwrap.

## 17. Non-Functional Requirements

**NFR-1.** MSRV is declared and enforced in CI. `#![forbid(unsafe_code)]` holds for all first-party crates.

**NFR-2.** Public API stability follows semantic versioning. Breaking changes to the neutral model require a major version.

**NFR-3.** Memory consumption per active stream is bounded by configuration, and the documented default limits are the values the code ships and are asserted by tests: `StreamLimits` caps one unfinished streamed part at 8 MiB and buffers 256 events, `ReadLimits` allows 16 MiB for one file, 64 KiB for one result, 2000 lines shown, 200 matches and 500 characters of one line, `EditLimits` 4 MiB for one file, 16 KiB for one block and 64 edits per call, and `WriteLimits` 16 MiB for one write. Each executor refuses what is over its limit before doing the work, and the edit executor's comparison budget bounds its CPU as well (FR-TOOL-EDIT-3).

**NFR-4.** The library is async-runtime agnostic at the trait boundary; the reference implementation uses tokio.

**NFR-5.** Logging is performed exclusively through `tracing`; no direct stdout/stderr writes from library code.

**NFR-6.** Reconstruction and its cache guarantees are cheap: it performs no network I/O, no vendor calls and no unbounded work proportional to history size beyond the linear walk the store already requires.

**NFR-7.** Executors run synchronously inside the async call they are given: work is never moved to a blocking pool to hide a cost, and every cost a request can ask for is bounded by configuration before the work starts — bytes by the byte limits, comparisons by the edit budget, process output by the command limits. Blocking I/O itself — a read, a rename, an `fsync` — is bounded by no CPU budget, which is why every byte it moves is bounded first (NFR-3). Only `command` observes cancellation (FR-TOOL-BASH); the file executors complete or fail within their limits, and a future dropped mid-call leaves the file as it was (FR-TOOL-FILE-1).

### 17.1 Trust Boundaries

**NFR-SEC-1.** Tool output is untrusted content. The text a tool returns is bytes from the file system or from a process, read by a model that cannot tell data from instruction, and a report is the one place where a file writes into the conversation. The file executors escape what a terminal would act on and clip what they show (FR-TOOL-READ-5), and the edit executor's snippets do the same (FR-TOOL-EDIT-4). What is *not* done, and is not claimed: an excerpt is not fenced or otherwise marked as data, so a host that renders tool output in a channel where its own text carries authority — an agent loop that feeds a result as a system message, a UI that evaluates markup — has made a choice this suite does not make for it. The result parts are typed and separable precisely so that choice stays the host's.

**NFR-SEC-2.** Execution is not authorization. The executors check arguments and limits, never intent (FR-POL-1, §10.4): a request that says a path is safe is a request, and whether the process may reach it is the sandbox's. Neither a path nor a program is confined here, and no executor in `norte-tools` is a privilege boundary.

**NFR-SEC-3.** The export envelope is trusted at the host's level and unauthenticated. It is versioned and rejected wholesale when the version is unknown (FR-EXP-3), and it restores what it carries: the messages, the phase and turn, the host metadata, the queue, the last turn's request options as an explicit field (FR-EXP-7) and — when the exported session was parked — the engine's own `norte.partial_tools` entry inside that metadata, so a message whose calls had already run does not run them again after a restore (FR-GRAPH-2, FR-APPR-8). It is not signed and carries no message authentication code, so an envelope is exactly as trustworthy as the place the host kept it. Approvals and grants never travel in it (FR-EXP-1): a restored session re-derives a pending approval from its head message's calls rather than trusting a record from the file.

**Verified by:** CI jobs **MSRV**, **Security Audit** and **Documentation** (`-D warnings`); `unsafe_code = "forbid"` in the workspace lints.

## 18. Testing & Conformance

**FR-TEST-1.** `norte-testing` supplies a scripted model that replays recorded fixtures, a request probe that captures reconstructed requests without a socket so wire bytes can be audited (FR-RECON-12, FR-EXP-6), and conformance suites that every adapter must pass. Recorded transcripts live under `norte-testing/fixtures/` and adapters consume them by compile-time inclusion, so a wire change that breaks decoding fails the build without network access.

**Where it lives:** `norte-testing/src/conformance.rs`, `norte-testing/src/probe.rs` (`guard`).

**FR-TEST-2.** Each shipped wire family has a recorded transcript; the adapter’s tests map it through the wire and pin the critical path — streamed text, usage (including cache read/write) and the finish reason — hermetically. Live smoke tests are `#[ignore]`d and key-gated, so the default run stays hermetic; a scheduled CI job runs them against the real APIs and fails the build on behavioural drift.

**Where it lives:** `norte-anthropic/src/messages/stream.rs` (`drain`), `norte-gemini/src/generate/stream.rs` (`decode_continuity`), `norte-openai/src/chat/stream.rs` (`drain`), `norte-openai/src/responses/stream.rs` (`drain`), `norte-testing/tests/live.rs`.

**FR-TEST-3.** Parsers for every wire format are fuzzed. Property tests cover continuity merge, graph commits, policy evaluation, and the reconstruction invariants: determinism and prefix stability (FR-RECON-1, FR-RECON-2, FR-RECON-11).

**Where it lives:** `norte-http/src/sse.rs` (`ignores_blocks_without_a_field`), `norte-testing/tests/properties.rs`.

**FR-TEST-4.** Cancellation, process-death and concurrent-writer scenarios are exercised by dedicated integration tests.

**Where it lives:** `norte-testing/tests/scenarios.rs`.

**FR-TEST-5.** Media materialization and `ProviderFile` expiry/re-upload are covered by tests that assert byte-stable output and a single, explicit invalidation on expiry.

**FR-TEST-6 — Exhaustive byte-identity matrix.** Byte identity is asserted at every level and on every vector, and any failing vector fails the build:
- **Envelope.** `Conversation` JSON serialize → deserialize → serialize over generated histories, metadata, queue and phase, including empty and maximal shapes; deserialize must not lose information and re-serialization must be byte-stable.
- **Export/import.** Export → import → re-export reproduces the conversation exactly, and import preserves the session identity (FR-EXP-4).
- **Request.** The reconstructed request is byte-identical before and after export/import, including the session-derived cache key (FR-RECON-12, FR-EXP-4).
- **Wire.** The adapter wire projection (FR-RECON-12) is byte-identical for every shipped wire family (Section 12).
- **Options.** The full generation-parameter matrix (temperature, top_p, top_k, max_output_tokens, stop_sequences, seed, output_schema, logprobs), every reasoning mode, representative tool subsets — including empty, single and maximal — and a vendor request extra round-trip through the request and are reported by the audit event (FR-ENG-7, FR-ENG-8, FR-ENG-9).

**Where it lives:** `norte-testing/tests/turn_options.rs`.

**FR-TEST-7 — Reconstruction edge cases.** The invariants that only appear away from the happy path are pinned by dedicated hermetic tests, comparing serialized bytes rather than modelled values:
- **Branches.** A head moved to an ancestor reproduces the shared prefix byte-for-byte, forks the graph into two children and leaves the ancestor immutable (FR-RECON-8).
- **Ephemeral tail.** An ephemeral message materializes after the whole critical prefix, never moves a cache boundary, and changing its content leaves the critical prefix unchanged (FR-RECON-5, FR-RECON-6).
- **Tool subset.** Changing the exposed set leaves the message sequence byte-identical and moves the request digest; naming the same set in another order reproduces the same request (FR-CACHE-5).
- **Continuity across models.** An exported signature is replayed by its exact model, dropped by a different model outside a tool loop, and blocks mid tool loop instead of being sent (FR-CONT-2, FR-CONT-4, FR-EXP-2).
- **Parked results.** A parked message’s completed results are visible under `norte.partial_tools` while it parks, are never run again on resume, and the key is cleared when the tool message commits (FR-APPR-8).
- **Expired provider files.** A `ProviderFile` that has expired is revived once from its blob, the revived reference is reused on every later turn so the prefix does not move, and the committed node keeps the reference it was written with (FR-PART-4, FR-RECON-4).
