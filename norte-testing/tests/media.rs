//! The engine's media pipeline: input preprocessing and materialization on the
//! way out, output ingestion and externalization on the way in (FR-PART-3,
//! FR-PART-5, FR-RECON-4).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use norte_core::{
    BlobId, BlobStore, Capabilities, EngineBuilder, EngineConfig, FinishReason, InMemoryBlobStore,
    InMemoryStore, MediaAdapter, MediaKind, MediaMetadata, MediaPart, MediaPayload,
    MediaPreprocessor, MediaSource, Message, MimeType, Modality, Part, ProviderFileRef,
    ProviderFiles, ProviderId, Result, Role, StreamEvent, StreamingGranularity, ToolRegistry,
};
use norte_http::{HttpClient, HttpMedia};
use norte_testing::{ScriptedModel, model};

/// Marks inline bytes so a test can see the pass ran.
struct Marker;

#[async_trait::async_trait]
impl MediaPreprocessor for Marker {
    async fn preprocess(&self, payload: MediaPayload) -> Result<MediaPayload> {
        let mut bytes = b"marker:".to_vec();
        bytes.extend(payload.bytes);
        Ok(MediaPayload::new(payload.kind, payload.mime, bytes))
    }
}

/// Counts both directions of the media seam.
#[derive(Default)]
struct Adapter {
    resolved: AtomicUsize,
    ingested: AtomicUsize,
}

#[async_trait::async_trait]
impl MediaAdapter for Adapter {
    async fn ingest(&self, part: MediaPart, _blobs: &dyn BlobStore) -> Result<MediaPart> {
        self.ingested.fetch_add(1, Ordering::Relaxed);
        Ok(part)
    }

    async fn resolve(&self, mut part: MediaPart, blobs: &dyn BlobStore) -> Result<MediaPart> {
        self.resolved.fetch_add(1, Ordering::Relaxed);
        if let MediaSource::Blob(id) = part.source {
            part.source = MediaSource::Inline(blobs.get(id).await?);
        }
        Ok(part)
    }
}

fn mime() -> MimeType {
    MimeType::new("image/png").expect("mime")
}

fn image(source: MediaSource) -> Part {
    Part::Media(MediaPart::new(MediaKind::Image, source, mime()))
}

fn inline_sources(messages: &[Message]) -> Vec<Vec<u8>> {
    messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            Part::Media(media) => match &media.source {
                MediaSource::Inline(bytes) => Some(bytes.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn input_media_is_preprocessed_then_materialized() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let stored = blobs.put(b"stored bytes").await.expect("put");
    let adapter = Arc::new(Adapter::default());
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .with_capabilities(
                Capabilities::builder()
                    .input(Modality::Text)
                    .input(Modality::Image)
                    .output(Modality::Text)
                    .streaming(StreamingGranularity::Delta)
                    .build(),
            )
            .turn_text("seen"),
    );
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(blobs)
        .model(scripted.clone())
        .tools(ToolRegistry::new())
        .media_preprocessor(Arc::new(Marker))
        .media_adapter(adapter.clone())
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    let mut inline = Message::user("inline");
    inline
        .parts
        .push(image(MediaSource::Inline(b"inline bytes".to_vec())));
    let stored_message = Message::new(Role::User, vec![image(MediaSource::Blob(stored))]);
    engine
        .append_history(session.id, vec![inline, stored_message])
        .await
        .expect("seed history");

    engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");

    let requests = scripted.requests();
    let last = requests.last().expect("a request");
    let sources = inline_sources(&last.messages);
    assert_eq!(sources.len(), 2, "both inputs reach the wire inline");
    assert_eq!(
        sources[0], b"marker:inline bytes",
        "the preprocessor reshapes inline input"
    );
    assert_eq!(
        sources[1], b"stored bytes",
        "a stored part is materialized from the blob store"
    );
    assert_eq!(adapter.resolved.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn output_media_is_ingested_and_externalized() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = Arc::new(Adapter::default());
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn(vec![
        StreamEvent::MediaStart {
            index: 0,
            kind: MediaKind::Image,
            mime: mime(),
            metadata: MediaMetadata::default(),
        },
        StreamEvent::MediaChunk {
            index: 0,
            bytes: vec![7; 64],
            last: true,
        },
        StreamEvent::Finish(FinishReason::Stop),
    ]));
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(blobs.clone())
        .model(scripted)
        .tools(ToolRegistry::new())
        .media_adapter(adapter.clone())
        .config(EngineConfig {
            inline_threshold: 8,
            ..EngineConfig::default()
        })
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("draw"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(adapter.ingested.load(Ordering::Relaxed), 1);
    let media = outcome
        .message
        .as_ref()
        .and_then(|message| {
            message.parts.iter().find_map(|part| match part {
                Part::Media(media) => Some(media.clone()),
                _ => None,
            })
        })
        .expect("the media part is committed");
    let MediaSource::Blob(id) = media.source else {
        panic!("over-threshold inline bytes must be externalized");
    };
    assert_eq!(
        blobs.get(id).await.expect("get"),
        vec![7; 64],
        "the committed part points at durable bytes"
    );
}

/// The vendor file port `HttpMedia` re-uploads through.
struct Files {
    bytes: Vec<u8>,
    uploads: AtomicUsize,
}

#[async_trait::async_trait]
impl ProviderFiles for Files {
    async fn download(&self, _reference: &ProviderFileRef) -> Result<Vec<u8>> {
        Ok(self.bytes.clone())
    }

    async fn upload(
        &self,
        _kind: MediaKind,
        _mime: &MimeType,
        bytes: &[u8],
    ) -> Result<ProviderFileRef> {
        let n = self.uploads.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(ProviderFileRef {
            provider: ProviderId::new("test").expect("provider"),
            file_id: format!("fresh-{n}"),
            expires_at: None,
            blob: Some(BlobId::of(bytes)),
        })
    }
}

fn expired_reference(blob: BlobId) -> ProviderFileRef {
    ProviderFileRef {
        provider: ProviderId::new("test").expect("provider"),
        file_id: "stale".to_owned(),
        expires_at: Some(chrono::DateTime::from_timestamp(0, 0).expect("a timestamp before now")),
        blob: Some(blob),
    }
}

#[tokio::test]
async fn an_expired_provider_file_is_revived_once_and_the_prefix_stays_stable() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let blob = blobs.put(b"document bytes").await.expect("put");
    let files = Arc::new(Files {
        bytes: b"document bytes".to_vec(),
        uploads: AtomicUsize::new(0),
    });
    let media = Arc::new(
        HttpMedia::new(&HttpClient::new().expect("client")).with_provider_files(files.clone()),
    );
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .with_capabilities(
                Capabilities::builder()
                    .input(Modality::Text)
                    .input(Modality::Image)
                    .output(Modality::Text)
                    .streaming(StreamingGranularity::Delta)
                    .build(),
            )
            .turn_text("one")
            .turn_text("two"),
    );
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(blobs)
        .model(scripted.clone())
        .tools(ToolRegistry::new())
        .media_adapter(media)
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    engine
        .append_history(
            session.id,
            vec![
                Message::new(
                    Role::User,
                    vec![image(MediaSource::ProviderFile(expired_reference(blob)))],
                ),
                Message::user("first"),
            ],
        )
        .await
        .expect("seed");

    engine
        .resume(session.id)
        .finish()
        .await
        .expect("first turn");
    engine
        .send(session.id, Message::user("second"))
        .finish()
        .await
        .expect("second turn");

    let requests = scripted.requests();
    assert_eq!(requests.len(), 2);
    let first = &requests[0];
    let second = &requests[1];

    // The expired reference is replaced on the way out, and the replacement is
    // the same on both turns: only the appended suffix is new bytes. The whole
    // prefix would otherwise be re-billed (FR-RECON-4, FR-RECON-3).
    let revived = first.messages[0]
        .media()
        .first()
        .map(|media| media.source.clone())
        .expect("the media part reaches the wire");
    let MediaSource::ProviderFile(fresh) = &revived else {
        panic!("the expired reference must be replaced by a fresh one");
    };
    assert_ne!(fresh.file_id, "stale");
    assert_eq!(
        serde_json::to_vec(&second.messages[..first.messages.len()]).expect("prefix"),
        serde_json::to_vec(&first.messages).expect("first request"),
        "the revived reference is reused, so the prefix does not move"
    );
    assert_eq!(
        files.uploads.load(Ordering::Relaxed),
        1,
        "the file is re-uploaded once and remembered by the adapter"
    );

    // The graph itself is untouched: the node keeps the reference it was written
    // with, and every reconstruction substitutes the fresh one (FR-GRAPH-2).
    let history = engine.history(session.id).await.expect("history");
    let stored = history[0]
        .media()
        .first()
        .map(|media| media.source.clone())
        .expect("stored media");
    let MediaSource::ProviderFile(original) = &stored else {
        panic!("the stored part is still a provider file");
    };
    assert_eq!(original.file_id, "stale");
}
