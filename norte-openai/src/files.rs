//! The `OpenAI` Files API, as a [`ProviderFiles`] port.

use chrono::DateTime;
use norte_core::{BlobId, Error, MediaKind, MimeType, ProviderFileRef, ProviderFiles, Result};
use norte_http::{HttpClient, MultipartField, VendorProfile};

/// Path of the Files collection, relative to the profile's base `URL`.
const COLLECTION: &str = "files";

/// The `purpose` the API requires on upload. `user_data` fits files kept for
/// reference over any endpoint, unlike `assistants` or `fine-tune`.
const DEFAULT_PURPOSE: &str = "user_data";

/// The `OpenAI` Files API as a [`ProviderFiles`] port.
///
/// It uploads bytes (`POST /files`) and fetches them back
/// (`GET /files/{id}/content`). It is not part of the request path: the host
/// attaches it to [`OpenAiMedia`](crate::OpenAiMedia) with
/// `with_provider_files`, so an output file can be durable-ized into a blob and
/// an expired reference revived from that blob — the wire adapter itself never
/// consults it. Vendors with no Files API, such as `OpenRouter`, leave it
/// unwired and keep media inline or by `URL`.
#[derive(Clone)]
pub struct OpenAiFiles {
    profile: VendorProfile,
    client: HttpClient,
    purpose: String,
}

impl OpenAiFiles {
    /// A Files port against `profile`, uploading with `purpose` `user_data`.
    ///
    /// # Errors
    ///
    /// Returns an error when the default `HTTP` client cannot be built.
    pub fn new(profile: VendorProfile) -> Result<Self> {
        Ok(Self {
            profile,
            client: HttpClient::new()?,
            purpose: DEFAULT_PURPOSE.to_owned(),
        })
    }

    /// Uses an existing transport instead of building one.
    #[must_use]
    pub fn with_client(mut self, client: HttpClient) -> Self {
        self.client = client;
        self
    }

    /// Uploads with `purpose` instead of `user_data`.
    #[must_use]
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = purpose.into();
        self
    }

    /// The profile this port sends through.
    #[must_use]
    pub fn profile(&self) -> &VendorProfile {
        &self.profile
    }
}

#[async_trait::async_trait]
impl ProviderFiles for OpenAiFiles {
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
        let fields = vec![
            MultipartField::File {
                name: "file".to_owned(),
                filename: filename(kind, mime),
                mime: mime.as_str().to_owned(),
                bytes: bytes.to_vec(),
            },
            MultipartField::Text {
                name: "purpose".to_owned(),
                value: self.purpose.clone(),
            },
        ];
        let body = self
            .client
            .send_post_multipart(&self.profile, COLLECTION, fields)
            .await?;
        let file_id = body
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Codec("openai files: the response carries no id".to_owned()))?;
        let expires_at = body
            .get("expires_at")
            .and_then(serde_json::Value::as_i64)
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
        Ok(ProviderFileRef {
            provider: self.profile.provider.clone(),
            file_id: file_id.to_owned(),
            expires_at,
            blob: Some(BlobId::of(bytes)),
        })
    }
}

/// The filename the vendor records. `OpenAI` mostly ignores it, but a
/// predictable one keeps the file list readable.
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

    use norte_core::{ProviderErrorKind, ProviderId, WireFamily};

    use super::*;

    fn profile(base: &str) -> VendorProfile {
        VendorProfile::new(
            ProviderId::new("openai").expect("provider"),
            WireFamily::new("openai.chat").expect("wire"),
            base,
        )
        .expect("profile")
    }

    fn mime(value: &str) -> MimeType {
        MimeType::new(value).expect("mime")
    }

    #[test]
    fn a_filename_keeps_the_media_extension() {
        assert_eq!(
            filename(MediaKind::Document, &mime("application/pdf")),
            "document.pdf"
        );
    }

    #[tokio::test]
    async fn a_file_round_trips_through_the_files_api() {
        let files = OpenAiFiles::new(profile(&serve(route))).expect("files");
        let reference = files
            .upload(MediaKind::Document, &mime("application/pdf"), b"report")
            .await
            .expect("upload");
        assert_eq!(reference.file_id, "file-abc");
        assert_eq!(reference.provider.as_str(), "openai");
        assert_eq!(reference.blob, Some(BlobId::of(b"report")));
        let bytes = files.download(&reference).await.expect("download");
        assert_eq!(bytes, b"report");
    }

    #[tokio::test]
    async fn a_vendor_without_a_files_api_surfaces_the_provider_error() {
        let files = OpenAiFiles::new(profile(&serve(not_implemented))).expect("files");

        let error = files
            .upload(MediaKind::Document, &mime("application/pdf"), b"report")
            .await
            .expect_err("the vendor has no Files API");

        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::NotFound));
    }

    /// A tiny `HTTP` server: it answers only the two Files routes, and only
    /// when the upload carries the `file` and `purpose` parts, so a wrong path
    /// or field name fails the round trip.
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
        if line.starts_with("POST /files ") {
            let body = String::from_utf8_lossy(body);
            return if body.contains("name=\"file\"") && body.contains("name=\"purpose\"") {
                (
                    "200 OK",
                    br#"{"id":"file-abc","bytes":6,"purpose":"user_data"}"#.to_vec(),
                )
            } else {
                ("400 Bad Request", b"{}".to_vec())
            };
        }
        if line.starts_with("GET /files/file-abc/content ") {
            return ("200 OK", b"report".to_vec());
        }
        ("404 Not Found", b"{}".to_vec())
    }

    /// A vendor with no Files API: every path is unknown.
    fn not_implemented(_line: &str, _body: &[u8]) -> (&'static str, Vec<u8>) {
        ("404 Not Found", b"{}".to_vec())
    }
}
