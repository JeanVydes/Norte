//! Mapping between MCP content and the neutral core domain.
//!
//! The mapping is deliberately total and lossless where the domain allows it.
//! Text becomes [`Part::Text`]; image and audio become inline [`MediaPart`]s;
//! embedded resources become a text part (carrying a citation for the URI) or an
//! inline media part; resource links become cited text. A content block whose
//! kind is unknown to this version is preserved verbatim as an
//! [`Part::Extension`] so a newer server is never silently lossy.

use std::sync::OnceLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use norte_core::{
    Citation, CitationSource, ExtensionPart, MediaKind, MediaPart, MediaSource, MimeType, Part,
    TextPart, ToolOutput,
};

use crate::error::{McpError, McpResult};
use crate::protocol::{CallToolResult, ContentBlock, ResourceContents};

/// Converts a `tools/call` result into the core tool envelope.
///
/// # Errors
///
/// Returns a protocol or base64 error when a content block cannot be mapped.
pub(crate) fn result_to_output(result: CallToolResult) -> McpResult<ToolOutput> {
    let mut parts = Vec::new();
    for block in result.content {
        if let Some(part) = content_to_part(block)? {
            parts.push(part);
        }
    }
    Ok(ToolOutput {
        parts,
        structured: result.structured_content,
        is_error: result.is_error,
    })
}

/// Converts the core tool envelope into a `tools/call` result.
pub(crate) fn output_to_result(output: ToolOutput) -> CallToolResult {
    let content = output.parts.into_iter().map(part_to_content).collect();
    CallToolResult {
        content,
        is_error: output.is_error,
        structured_content: output.structured,
        extra: std::collections::BTreeMap::new(),
    }
}

/// Converts one content block into a domain part.
///
/// Returns `Ok(None)` for a block that carries no content (an empty embedded
/// resource), and preserves unknown block kinds as extensions.
///
/// # Errors
///
/// Returns a protocol error for a malformed known block and a base64 error when
/// an inline payload cannot be decoded.
pub(crate) fn content_to_part(block: ContentBlock) -> McpResult<Option<Part>> {
    match block.kind.as_str() {
        "text" => Ok(Some(Part::Text(TextPart::new(
            block.text.unwrap_or_default(),
        )))),
        "image" => Ok(Some(Part::Media(base64_media(MediaKind::Image, &block)?))),
        "audio" => Ok(Some(Part::Media(base64_media(MediaKind::Audio, &block)?))),
        "resource" => match block.resource {
            Some(contents) => Ok(Some(resource_to_part(contents)?)),
            None => Ok(None),
        },
        "resource_link" => Ok(Some(resource_link_to_part(&block))),
        _ => Ok(Some(Part::Extension(ExtensionPart {
            namespace: "mcp".to_owned(),
            data: serde_json::to_value(&block)?,
        }))),
    }
}

/// Converts one domain part into a content block.
pub(crate) fn part_to_content(part: Part) -> ContentBlock {
    match part {
        Part::Text(text) => ContentBlock::text(text.text),
        Part::Media(media) => media_to_content(media),
        other => ContentBlock::text(render_text(&other)),
    }
}

fn render_text(part: &Part) -> String {
    match part.as_text() {
        Some(text) => text.to_owned(),
        None => format!("{part:?}"),
    }
}

fn media_to_content(media: MediaPart) -> ContentBlock {
    match media.source {
        MediaSource::Inline(bytes) => {
            let encoded = STANDARD.encode(&bytes);
            match media.kind {
                MediaKind::Image => ContentBlock::image(encoded, media.mime.as_str()),
                MediaKind::Audio => ContentBlock::audio(encoded, media.mime.as_str()),
                _ => {
                    let uri = format!("data:{};base64,{encoded}", media.mime);
                    ContentBlock::resource(ResourceContents::blob(
                        uri,
                        media.mime.as_str(),
                        encoded,
                    ))
                }
            }
        }
        MediaSource::Url(url) => ContentBlock::resource_link(url, media.mime.as_str()),
        MediaSource::Blob(id) => ContentBlock::text(format!("[blob {id} ({})]", media.mime)),
        MediaSource::ProviderFile(file) => {
            ContentBlock::text(format!("[provider file {} ({})]", file.file_id, media.mime))
        }
        other => ContentBlock::text(format!("[media {other:?}]")),
    }
}

fn base64_media(kind: MediaKind, block: &ContentBlock) -> McpResult<MediaPart> {
    let data = block
        .data
        .as_deref()
        .ok_or_else(|| McpError::Protocol(format!("{} block has no data payload", block.kind)))?;
    let bytes = STANDARD.decode(data)?;
    let mime = match block.mime_type.as_deref() {
        Some(mime) => MimeType::new(mime)?,
        None => octet_stream(),
    };
    Ok(MediaPart::new(kind, MediaSource::Inline(bytes), mime))
}

fn resource_to_part(contents: ResourceContents) -> McpResult<Part> {
    if let Some(text) = contents.text {
        return Ok(Part::Text(TextPart {
            text,
            citations: vec![uri_citation(&contents.uri)],
            continuity: None,
        }));
    }
    if let Some(blob) = contents.blob {
        let bytes = STANDARD.decode(blob)?;
        let mime = match contents.mime_type.as_deref() {
            Some(mime) => MimeType::new(mime)?,
            None => octet_stream(),
        };
        let kind = kind_for_mime(mime.as_str());
        return Ok(Part::Media(MediaPart::new(
            kind,
            MediaSource::Inline(bytes),
            mime,
        )));
    }
    Ok(Part::Text(TextPart::new(contents.uri)))
}

fn resource_link_to_part(block: &ContentBlock) -> Part {
    let uri = block.uri.clone().unwrap_or_default();
    let title = block.name.clone().or_else(|| block.title.clone());
    let text = title.clone().unwrap_or_else(|| uri.clone());
    Part::Text(TextPart {
        text,
        citations: vec![Citation {
            source: CitationSource::Url(uri),
            title,
        }],
        continuity: None,
    })
}

fn uri_citation(uri: &str) -> Citation {
    Citation {
        source: CitationSource::Url(uri.to_owned()),
        title: None,
    }
}

fn kind_for_mime(mime: &str) -> MediaKind {
    match mime.split('/').next().unwrap_or_default() {
        "image" => MediaKind::Image,
        "audio" => MediaKind::Audio,
        "video" => MediaKind::Video,
        _ => MediaKind::Document,
    }
}

fn octet_stream() -> MimeType {
    static FALLBACK: OnceLock<MimeType> = OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            MimeType::new("application/octet-stream").expect("the fallback MIME type is valid")
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_and_media_round_trip() {
        let result = CallToolResult::new(vec![
            ContentBlock::text("hello"),
            ContentBlock::image("AAAA", "image/png"),
        ]);
        let output = result_to_output(result).expect("map");
        assert_eq!(output.parts.len(), 2);
        assert!(matches!(&output.parts[0], Part::Text(text) if text.text == "hello"));
        match &output.parts[1] {
            Part::Media(media) => {
                assert_eq!(media.kind, MediaKind::Image);
                assert_eq!(media.inline_len(), 3);
            }
            other => panic!("unexpected part: {other:?}"),
        }
        let back = output_to_result(output);
        assert_eq!(back.content.len(), 2);
        assert_eq!(back.content[0].kind, "text");
        assert_eq!(back.content[1].kind, "image");
    }

    #[test]
    fn unknown_blocks_are_preserved_as_extensions() {
        let block: ContentBlock =
            serde_json::from_value(json!({ "type": "video", "url": "https://x" })).expect("decode");
        let part = content_to_part(block).expect("map").expect("part");
        match part {
            Part::Extension(extension) => {
                assert_eq!(extension.namespace, "mcp");
                assert_eq!(extension.data["type"], "video");
            }
            other => panic!("unexpected part: {other:?}"),
        }
    }

    #[test]
    fn embedded_text_resources_carry_a_uri_citation() {
        let block = ContentBlock::resource(ResourceContents::text("file://a", "body"));
        let part = content_to_part(block).expect("map").expect("part");
        match part {
            Part::Text(text) => {
                assert_eq!(text.text, "body");
                assert_eq!(text.citations.len(), 1);
            }
            other => panic!("unexpected part: {other:?}"),
        }
    }

    #[test]
    fn structured_content_and_error_survive() {
        let result = CallToolResult {
            content: vec![ContentBlock::text("boom")],
            is_error: true,
            structured_content: Some(json!({ "code": 3 })),
            extra: std::collections::BTreeMap::new(),
        };
        let output = result_to_output(result).expect("map");
        assert!(output.is_error);
        assert_eq!(output.structured, Some(json!({ "code": 3 })));
    }
}
