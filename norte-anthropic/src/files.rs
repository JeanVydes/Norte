//! The `Anthropic` Files API, as a [`ProviderFiles`](norte_core::ProviderFiles) port.

use chrono::{DateTime, Utc};
use norte_core::{BlobId, Error, MediaKind, MimeType, ProviderFileRef, ProviderFiles, Result};
use norte_http::{HttpClient, MultipartField, VendorProfile};

/// Path of the Files collection, relative to the profile's base `URL`.
const COLLECTION: &str = "files";

/// The `Anthropic` Files API as a [`ProviderFiles`] port.
///
/// It uploads bytes (`POST /files`) and fetches them back
/// (`GET /files/{id}/content`). Like the `OpenAI` port, it is not part of the
/// request path: the host attaches it to
/// [`AnthropicMedia`](crate::AnthropicMedia) with `with_provider_files` so
/// output files are durable-ized and expired references revived.
///
/// The profile must carry the `anthropic-version` header the API requires; the
/// [`anthropic_profile`](crate::presets::anthropic_profile) preset sets it.
///
/// `Anthropic` only serves `GET /files/{id}/content` for files it generated
/// itself — a file the host uploaded is not downloadable and the endpoint
/// answers `400`. [`download`](norte_core::ProviderFiles::download) therefore fails for an
/// uploaded reference; the fallback blob on the reference is what makes such a
/// reference revivable.
#[derive(Clone)]
pub struct AnthropicFiles {
    profile: VendorProfile,
    client: HttpClient,
}

impl AnthropicFiles {
    /// A Files port against `profile`.
    ///
    /// # Errors
    ///
    /// Returns an error when the default `HTTP` client cannot be built.
    pub fn new(profile: VendorProfile) -> Result<Self> {
        Ok(Self {
            profile,
            client: HttpClient::new()?,
        })
    }

    /// Uses an existing transport instead of building one.
    #[must_use]
    pub fn with_client(mut self, client: HttpClient) -> Self {
        self.client = client;
        self
    }

    /// The profile this port sends through.
    #[must_use]
    pub fn profile(&self) -> &VendorProfile {
        &self.profile
    }
}

#[async_trait::async_trait]
impl ProviderFiles for AnthropicFiles {
    async fn download(&self, reference: &ProviderFileRef) -> Result<Vec<u8>> {
        let path = format!("{COLLECTION}/{}/content", reference.file_id);
        self.client.send_get_bytes(&self.profile, &path).await
    }

    async fn upload(
        &self,
        kind: MediaKind,
        mime: &MimeType,
        bytes: &[u8],
    ) -> Result<ProviderFileRef> {
        let fields = vec![MultipartField::File {
            name: "file".to_owned(),
            filename: filename(kind, mime),
            mime: mime.as_str().to_owned(),
            bytes: bytes.to_vec(),
        }];
        let body = self
            .client
            .send_post_multipart(&self.profile, COLLECTION, fields)
            .await?;
        let file_id = body
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::Codec("anthropic files: the response carries no id".to_owned())
            })?;
        let expires_at = body
            .get("expires_at")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        Ok(ProviderFileRef {
            provider: self.profile.provider.clone(),
            file_id: file_id.to_owned(),
            expires_at,
            blob: Some(BlobId::of(bytes)),
        })
    }
}

/// The filename the vendor records and echoes back in its metadata.
fn filename(kind: MediaKind, mime: &MimeType) -> String {
    let extension = mime
        .as_str()
        .split_once('/')
        .map_or("bin", |(_, subtype)| subtype);
    let stem = match kind {
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
        MediaKind::Document => "document",
        _ => "file",
    };
    format!("{stem}.{extension}")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use norte_core::{ProviderId, WireFamily};

    use super::*;

    fn profile(base: &str) -> VendorProfile {
        VendorProfile::new(
            ProviderId::new("anthropic").expect("provider"),
            WireFamily::new("anthropic.messages").expect("wire"),
            base,
        )
        .expect("profile")
    }

    fn mime(value: &str) -> MimeType {
        MimeType::new(value).expect("mime")
    }

    #[test]
    fn a_filename_keeps_the_media_extension() {
        assert_eq!(filename(MediaKind::Image, &mime("image/png")), "image.png");
    }

    #[tokio::test]
    async fn a_file_round_trips_through_the_files_api() {
        let base = format!("{}/v1", serve(route));
        let files = AnthropicFiles::new(profile(&base)).expect("files");
        let reference = files
            .upload(MediaKind::Document, &mime("application/pdf"), b"paper")
            .await
            .expect("upload");
        assert_eq!(reference.file_id, "file_abc");
        assert_eq!(reference.provider.as_str(), "anthropic");
        assert_eq!(reference.blob, Some(BlobId::of(b"paper")));
        assert!(reference.expires_at.is_some());
        let bytes = files.download(&reference).await.expect("download");
        assert_eq!(bytes, b"paper");
    }

    /// See the sibling test in `norte-openai`: it answers only the two Files
    /// routes, and only for a well-formed `file` part.
    fn serve(route: fn(&str, &[u8]) -> (&'static str, Vec<u8>)) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let (status, body) = route(&line, &body);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{address}")
    }

    fn route(line: &str, body: &[u8]) -> (&'static str, Vec<u8>) {
        if line.starts_with("POST /v1/files ") {
            let body = String::from_utf8_lossy(body);
            return if body.contains("name=\"file\"") {
                (
                    "200 OK",
                    br#"{"id":"file_abc","type":"file","expires_at":"2026-01-01T00:00:00Z"}"#
                        .to_vec(),
                )
            } else {
                ("400 Bad Request", b"{}".to_vec())
            };
        }
        if line.starts_with("GET /v1/files/file_abc/content ") {
            return ("200 OK", b"paper".to_vec());
        }
        ("404 Not Found", b"{}".to_vec())
    }
}
