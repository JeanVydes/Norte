# Glossary

**Cache boundary** — a position in a request up to which the provider may cache.
Announced in `RequestReconstructed::cache_boundaries`; the provider decides what
it honours.

**CacheClass** — the marker on a `Message` that says where a cache boundary may
fall. It is a class, not a cache: the engine declares, the provider decides.

**Capabilities** — the host's declaration of what a model can do (modalities,
tool kind, streaming granularity, cache mode, limits). Checked before a request is
built. See [Models and capabilities](../concepts/models-and-capabilities.md).

**Commit** — the single atomic write that ends a turn: messages, phase, version,
head and any new approval, together. Compare-and-swap on `expected_version`.

**Continuity token** — an opaque blob a provider hands back (a thought signature,
an encrypted reasoning block) that must be returned unchanged on the next request.
Carried on a `Part` as `continuity`.

**Conversation** — the portable, versioned document produced by `Engine::export`
and consumed by `Engine::import`. Deterministic and append-only.

**Digest** — a content hash. `RequestReconstructed::digest` is the hash of a
whole assembled request.

**Engine** — the object that owns the store, the blob store, the model, the tool
registry, the policies, the layers and the observers, and runs turns.

**EngineEvent** — the lifecycle of a turn: phase changes, deltas, part
completions, tool starts and finishes, approvals, commits, request reconstruction,
errors. See [Streaming](../concepts/streaming.md).

**Grant** — a live tool approval, with a scope (`Call`, `Tool`, `Session`,
`Global`) and the actor who issued it.

**Layer** — a wrapper around the model call that can rewrite a request, answer it
locally, or pass it on. See [Layers and observers](../concepts/layers-and-observers.md).

**Logprob** — the probability a model assigned to a token, wanted by evaluation
and guardrail hosts and rarely by others. Opt-in per turn (`logprobs(n)`), gated
on `Feature::Logprobs`, delivered as a `Logprobs` delta and never written to the
conversation. See [Streaming](../concepts/streaming.md).

**Message** — a `Role` plus a list of `Part`s, plus an optional origin (which
carries `Usage`).

**Model** — the port (`dyn Model`) that turns a request into a stream of events.
A wire adapter, a scripted test model, or your own.

**ModelRef** — the `provider/model` pair that names who answers. A label, not a
lookup key.

**Node** — a commit in the session's tree, with a parent. The tree is what makes
branching and rewinding possible.

**Observer** — a passive watcher of every `EngineEvent`. Cannot change the turn.

**Part** — a typed piece of a message: text, reasoning, media, tool call, tool
result, provider-executed block, refusal, or an extension kept verbatim.

**Phase** — a session's state: `Idle`, `Streaming`, `AwaitingTool`,
`AwaitingApproval`.

**Policy** — a function over a `PolicyView` returning a `Decision`: allow, deny,
transform the arguments, require approval, or rate-limit.

**ProviderId** — the routing/labelling name of a provider, `[a-z0-9_-]`.

**Request reconstruction** — the assembling of a `ModelRequest` from committed
history, announced by `RequestReconstructed` once per model request.

**Session** — the durable unit: a tree of commits, a phase, a version, a turn
counter, grants and metadata.

**Store / BlobStore** — the two persistence ports. `Store` holds sessions, nodes
and approvals; `BlobStore` holds content-addressed bytes.

**StreamDelta** — a piece of the model's output as it arrives: text, reasoning,
refusal, tool arguments, media, continuity, safety ratings, log probabilities.

**StreamTransform** — a rewrite of `StreamDelta`s between the provider and your
loop. Can drop or change a delta.

**ToolDeclaration / ToolSpec** — what you register and what the model is shown.
A spec is rendered per wire, with `$ref`s expanded and tags written as
discriminators.

**Turn** — one round of the model: input, events, one commit. A turn may contain
several model requests when tools are involved; `max_iterations` bounds them.

**Usage** — the provider's token counters for one request, plus a `vendor` map of
everything else it reported — including a price, if it reported one. The engine
never prices anything.

**Wire / WireFamily** — the protocol family an adapter speaks, e.g.
`openai.chat`, `openai.responses`, `anthropic.messages`, `google.gemini`.
