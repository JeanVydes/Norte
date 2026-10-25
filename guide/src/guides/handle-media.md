# Handle media

An image, a PDF, a voice note: media travels as a `Part::Media(MediaPart)`, and
the point of the abstraction is that the *bytes* and the *reference to the
bytes* are separate. A part says what the bytes are and where they live; the
engine moves them to the wire on the way out.

```rust,ignore
use norte_core::{MediaKind, MediaPart, MediaSource, MimeType};

let mime = MimeType::new("image/png")?;
let part = MediaPart::new(MediaKind::Image, MediaSource::Inline(png.clone()), mime);
```

## The four sources

| Source | Holds | Reaches the wire as |
| --- | --- | --- |
| `Inline(Vec<u8>)` | The bytes themselves (base64 in JSON). | The bytes. |
| `Blob(BlobId)` | An id into the `BlobStore`. | The bytes, read back and rebuilt. |
| `Url(String)` | A link. | The link, which the vendor fetches. |
| `ProviderFile(ProviderFileRef)` | A file the vendor already holds. | The vendor's id. |

A `Blob` and an `Inline` carry the same bytes; they differ in who owns them. A
`Url` and a `ProviderFile` carry none of them — they name a file on the vendor's
side — so they can go stale, and that is what the media adapter is for.

## Blobs and the inline threshold

Above the inline threshold — 64 KiB by default, `HarnessConfig::inline_threshold`
by way of `HarnessBuilder::config` — the engine writes inline input bytes into
the blob store and rewrites the part to a `Blob` before the message is
committed, so a graph node stays small and cheap to load (FR-PART-2). Wire the
store and it does this for you:

```rust,ignore
use std::sync::Arc;
use norte_harness::{HarnessBuilder, InMemoryBlobStore};

let engine = HarnessBuilder::new()
    .store(store)
    .blobs(Arc::new(InMemoryBlobStore::new()))
    .model(model)
    .build()?;
```

Model-generated media is externalized the other way: when a vendor streams
audio or image output, the bytes land in the blob store when the part ends
(FR-PART-3), so the graph never depends on an ephemeral URL.

## The media adapter

`HttpMedia` is the adapter that speaks the two directions:

- **resolve** — before a request goes out, a `Blob` is read back and a `Url` is
  fetched, so the vendor receives bytes (or a link it can fetch); an expired
  `ProviderFile` is revived from its blob.
- **ingest** — when a model *produces* media as a `Url` or a `ProviderFile`
  rather than stream chunks, the bytes are copied into the blob store before the
  assistant message is committed (FR-PART-5), so the graph holds a durable
  `Blob` rather than a link that expires.

Attach it once:

```rust,ignore
use std::sync::Arc;
use norte_http::{HttpClient, HttpMedia};

let media = Arc::new(HttpMedia::new(&HttpClient::new()?));
let engine = HarnessBuilder::new()
    /* .. */
    .media_adapter(media)
    .build()?;
```

Because every byte is re-derived on each request, the request prefix a provider
caches does not move when a URL is refreshed or a file is re-uploaded
(FR-RECON-3).

## Provider files

A `ProviderFileRef` is `{ provider, file_id, expires_at, blob }`. The `file_id`
is the vendor's handle; `blob` is the host's own durable copy, which is what lets
an expired reference be rebuilt without the vendor.

The port has two directions and is implemented by the three adapters that have a
file API — `norte_openai::OpenAiFiles`, `norte_anthropic::AnthropicFiles` and
`norte_gemini::GeminiFiles`:

```rust,ignore
use std::sync::Arc;
use norte_http::{HttpClient, HttpMedia};
use norte_openai::{OpenAiFiles, presets::openai_profile};

let vendor = Arc::new(OpenAiFiles::new(openai_profile(api_key)?)?);
let media = Arc::new(
    HttpMedia::new(&HttpClient::new()?).with_provider_files(vendor.clone()),
);

// The host keeps its own handle, too.
let reference = vendor.upload(MediaKind::Image, &mime, &png).await?;
```

Three rules govern the port:

- **Expiry is handled, or it fails loudly.** An expired reference with a blob is
  re-uploaded and the request goes out with a fresh id. An expired reference
  with no blob — or a vendor with no port wired at all — fails with a typed
  error naming the part, never a request that is silently missing its media
  (FR-PART-4).
- **No file API, no port.** OpenRouter carries images inline or by URL and has
  no Files API, so a host wires no port for it. Calling `upload` against such a
  vendor anyway surfaces the vendor's own typed error — a `404` read as
  `NotFound` — rather than an empty reference (FR-ADP-4).
- **A direction may not exist.** Anthropic and Gemini let a host download only
  files the vendor *generated*, not one it uploaded; the adapter reports the
  vendor's refusal instead of pretending the direction works.

## What each wire can carry

A kind a wire cannot express is a typed error, never a silent omission:

| Media kind | Gemini | Anthropic | OpenAI Responses | OpenAI Chat |
| --- | --- | --- | --- | --- |
| Image | `inlineData` / `fileData` | `image` | `input_image` | `image_url` |
| Document | `inlineData` / `fileData` | `document` | `input_file` | `file` |
| Audio | `inlineData` / `fileData` | — typed error | `input_file` | `input_audio` (inline `wav` / `mp3`) |
| Video | `inlineData` / `fileData` | — typed error | `input_file` | — typed error |

A media part inside a tool result rides the same wires — the materializer
descends into the result's parts — except that a wire whose tool-result field is
text-only refuses it with a typed capability error rather than dropping the
media (FR-TOOL-3).

## Try it

Two runnable recipes cover this end to end:

- `examples/cookbook/examples/media/media/main.rs` — inline, blob, URL ingestion and a tool
  result carrying an image, against OpenRouter.
- `examples/cookbook/examples/media/provider_files/main.rs` — the vendor-file lifecycle
  alone: upload, download, revive an expired reference, and what happens when a
  vendor has no file API.
