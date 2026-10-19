//! The `Google` Gemini Files API, as a [`ProviderFiles`] port.

use chrono::{DateTime, Utc};
use norte_core::{BlobId, Error, MediaKind, MimeType, ProviderFileRef, ProviderFiles, Result};
use norte_http::{HttpClient, MultipartField, VendorProfile};

/// Path appended to the derived upload root, for a single-shot upload.
const UPLOAD_PATH: &str = "files?uploadType=multipart";
/// Suffix, including the download root, that names a stored file's bytes.
const DOWNLOAD_SUFFIX: &str = ":download?alt=media";

/// The `Google` Gemini Files API as a [`ProviderFiles`] port.
///
/// Uploads bytes through the Files API's single-shot `uploadType=multipart`
/// path and fetches them back with `files/{id}:download?alt=media`. Like the
/// other ports it is not part of the request path: the host attaches it to
/// [`GeminiMedia`](crate::GeminiMedia) with `with_provider_files`.
///
/// `Gemini` stores uploaded files for 48 hours and reports the deadline as
/// `expirationTime`, which becomes the reference's `expires_at` so the engine
/// re-uploads from the fallback blob before it lapses. As on the other
/// vendors, only files the service *generated* are downloadable —
/// [`download`](ProviderFiles::download) of an uploaded reference is refused
/// server-side.
///
/// The port assumes the profile's base `URL` ends in the API version segment,
/// `.../v1beta`, the shape the [`gemini_profile`](crate::presets::gemini_profile)
/// preset fixes: the upload and download roots are siblings of it, `.../upload/v1beta`
/// and `.../download/v1beta`.
#[derive(Clone)]
pub struct GeminiFiles {
    profile: VendorProfile,
    upload: VendorProfile,
    download: VendorProfile,
    client: HttpClient,
}

impl GeminiFiles {
    /// A Files port against `profile`.
    ///
    /// # Errors
    ///
    /// Returns an error when the default `HTTP` client cannot be built.
    pub fn new(profile: VendorProfile) -> Result<Self> {
        Ok(Self {
            upload: shifted(&profile, "upload"),
            download: shifted(&profile, "download"),
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
impl ProviderFiles for GeminiFiles {
    async fn download(&self, reference: &ProviderFileRef) -> Result<Vec<u8>> {
        let path = format!("{}{DOWNLOAD_SUFFIX}", resource_name(&reference.file_id));
        self.client.send_get_bytes(&self.download, &path).await
    }

    async fn upload(
        &self,
        kind: MediaKind,
        mime: &MimeType,
        bytes: &[u8],
    ) -> Result<ProviderFileRef> {
        let name = filename(kind, mime);
        let metadata = serde_json::json!({ "file": { "displayName": name } });
        let fields = vec![
            MultipartField::Json {
                name: "metadata".to_owned(),
                value: metadata.to_string(),
            },
            MultipartField::File {
                name: "file".to_owned(),
                filename: name,
                mime: mime.as_str().to_owned(),
                bytes: bytes.to_vec(),
            },
        ];
        let body = self
            .client
            .send_post_multipart(&self.upload, UPLOAD_PATH, fields)
            .await?;
        let file = body.get("file").unwrap_or(&body);
        let uri = file
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Codec("gemini files: the response carries no uri".to_owned()))?;
        let expires_at = file
            .get("expirationTime")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        Ok(ProviderFileRef {
            // The `generateContent` wire puts `file_id` straight into
            // `fileData.fileUri`, so for `Gemini` the reference holds the full
            // file `URI`, not a bare id.
            provider: self.profile.provider.clone(),
            file_id: uri.to_owned(),
            expires_at,
            blob: Some(BlobId::of(bytes)),
        })
    }
}

/// A profile like `profile` but rooted at `.../{segment}/v1beta` instead of
/// `.../v1beta`: the Files API's upload and download roots are siblings of the
/// model base, keeping the version segment.
fn shifted(profile: &VendorProfile, segment: &str) -> VendorProfile {
    let path = profile.base_url.path().trim_end_matches('/');
    let (parent, version) = path.rsplit_once('/').unwrap_or(("", path));
    let mut shifted = profile.clone();
    shifted
        .base_url
        .set_path(&format!("{parent}/{segment}/{version}"));
    shifted
}

/// The Files API resource name, `files/{id}`, from whatever a reference holds:
/// a bare id, the full file `URI`, or the name itself.
fn resource_name(file_id: &str) -> String {
    let id = file_id
        .split('/')
        .next_back()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(file_id);
    format!("files/{id}")
}

/// The display name the vendor records and echoes back in its metadata.
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
            ProviderId::new("google").expect("provider"),
            WireFamily::new("google.gemini").expect("wire"),
            base,
        )
        .expect("profile")
    }

    fn mime(value: &str) -> MimeType {
        MimeType::new(value).expect("mime")
    }

    #[test]
    fn the_files_roots_are_siblings_of_the_model_base() {
        let base = profile("https://example.test/v1beta");
        assert_eq!(
            shifted(&base, "upload").base_url.as_str(),
            "https://example.test/upload/v1beta"
        );
        assert_eq!(
            shifted(&base, "download").base_url.as_str(),
            "https://example.test/download/v1beta"
        );
    }

    #[test]
    fn a_resource_name_is_derived_from_a_uri_or_id() {
        assert_eq!(
            resource_name("https://example.test/v1beta/files/abc"),
            "files/abc"
        );
        assert_eq!(resource_name("files/abc"), "files/abc");
        assert_eq!(resource_name("abc"), "files/abc");
    }

    #[tokio::test]
    async fn a_file_round_trips_through_the_files_api() {
        let base = format!("{}/v1beta", serve(route));
        let files = GeminiFiles::new(profile(&base)).expect("files");
        let reference = files
            .upload(MediaKind::Audio, &mime("audio/mpeg"), b"clip")
            .await
            .expect("upload");
        assert_eq!(reference.file_id, "https://example.test/v1beta/files/abc");
        assert_eq!(reference.provider.as_str(), "google");
        assert_eq!(reference.blob, Some(BlobId::of(b"clip")));
        assert!(reference.expires_at.is_some());
        let bytes = files.download(&reference).await.expect("download");
        assert_eq!(bytes, b"clip");
    }

    /// See the sibling test in `norte-openai`: it answers only the derived
    /// upload and download routes, and only for a metadata-plus-file body.
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
        if line.starts_with("POST /upload/v1beta/files") {
            let body = String::from_utf8_lossy(body);
            return if body.contains("name=\"metadata\"") && body.contains("name=\"file\"") {
                (
                    "200 OK",
                    br#"{"file":{"name":"files/abc","uri":"https://example.test/v1beta/files/abc","expirationTime":"2026-01-01T00:00:00Z"}}"#.to_vec(),
                )
            } else {
                ("400 Bad Request", b"{}".to_vec())
            };
        }
        if line.starts_with("GET /download/v1beta/files/abc:download") {
            return ("200 OK", b"clip".to_vec());
        }
        ("404 Not Found", b"{}".to_vec())
    }
}
