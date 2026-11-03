use std::time::Duration;

use norte_core::{Error, ProviderError, ProviderErrorKind, Result};

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// One server-sent event, already decoded from the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The event's name, when the stream named one.
    pub event: Option<String>,
    /// The event's data lines, joined by newlines.
    pub data: String,
    /// The stream's last event id, when it set one.
    pub id: Option<String>,
    /// A reconnection delay the stream asked for, when it asked.
    pub retry: Option<Duration>,
}

/// Incremental `SSE` parser. Feed it raw chunks in any split; it returns the
/// events that completed, keeping the remainder buffered.
///
/// The parser is transport-free and pure, so every vendor mapping can be tested
/// without a socket. Frames are limited to 8 MiB, excluding the blank-line
/// delimiter. [`Self::push`] returns a decode error for oversized input.
/// A rejected decoder stays failed; it never resumes with truncated data.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    failed: bool,
}

impl SseDecoder {
    /// A decoder with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a chunk without ever buffering more than one bounded frame.
    /// Returns completed events in wire order, followed by a terminal decode
    /// error if a frame exceeds 8 MiB, even when no delimiter arrives. Events
    /// completed before that error are preserved. After failure, returns an
    /// empty batch without processing further input.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<SseEvent>> {
        if self.failed {
            return Vec::new();
        }
        let mut events = Vec::new();
        // Delimiters can only end at a newline. Append whole line slices and
        // inspect their tails, rather than checking every byte or rescanning the
        // buffered frame when the next chunk arrives.
        for line in chunk.split_inclusive(|&byte| byte == b'\n') {
            if line.len() > MAX_FRAME_BYTES + 4 - self.buffer.len() {
                self.failed = true;
                self.buffer.clear();
                events.push(Err(oversized()));
                return events;
            }
            self.buffer.extend_from_slice(line);
            let separator = if self.buffer.ends_with(b"\n\n") {
                2
            } else if self.buffer.ends_with(b"\r\n\r\n") {
                4
            } else {
                0
            };
            // A delimiter prefix may follow a maximum-sized frame across chunks.
            let pending = if separator > 0 {
                separator
            } else if self.buffer.ends_with(b"\r\n\r") {
                3
            } else if self.buffer.ends_with(b"\r\n") {
                2
            } else {
                usize::from(matches!(self.buffer.last(), Some(b'\r' | b'\n')))
            };
            if self.buffer.len() - pending > MAX_FRAME_BYTES {
                self.failed = true;
                self.buffer.clear();
                events.push(Err(oversized()));
                return events;
            }
            if separator > 0 {
                if let Some(event) = parse_block(&self.buffer[..self.buffer.len() - separator]) {
                    events.push(Ok(event));
                }
                self.buffer.clear();
            }
        }
        events
    }

    /// Ends the stream. An unterminated event is not dispatched, per `SSE`.
    ///
    /// # Errors
    ///
    /// Returns a decode error for a previously rejected frame or an oversized
    /// final frame whose trailing bytes could have begun a delimiter.
    pub fn finish(&mut self) -> Result<()> {
        if self.failed || self.buffer.len() > MAX_FRAME_BYTES {
            self.failed = true;
            self.buffer.clear();
            return Err(oversized());
        }
        self.buffer.clear();
        Ok(())
    }

    /// Whether the buffer holds a partial event.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

fn oversized() -> Error {
    Error::provider(ProviderError::new(
        ProviderErrorKind::Decode,
        "SSE frame exceeds the 8 MiB limit",
    ))
}

fn parse_block(block: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(block);
    let mut event = SseEvent::default();
    let mut data = Vec::new();
    let mut seen = false;

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => {
                event.event = Some(value.to_owned());
                seen = true;
            }
            "data" => {
                data.push(value.to_owned());
                seen = true;
            }
            "id" => {
                event.id = Some(value.to_owned());
                seen = true;
            }
            "retry" => {
                if let Ok(millis) = value.parse::<u64>() {
                    event.retry = Some(Duration::from_millis(millis));
                    seen = true;
                }
            }
            _ => {}
        }
    }

    if !seen {
        return None;
    }
    event.data = data.join("\n");
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_events_with_multiline_data() {
        let mut decoder = SseDecoder::new();
        let events = decoder
            .push(b"event: message_start\ndata: {\"a\":\ndata: 1}\n\n")
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("valid frame");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[0].data, "{\"a\":\n1}");
    }

    #[test]
    fn survives_arbitrary_chunk_boundaries() {
        let mut decoder = SseDecoder::new();
        assert!(
            decoder.push(b"data: he").is_empty(),
            "a partial chunk completes no event"
        );
        assert!(
            decoder.push(b"llo\n").is_empty(),
            "a partial chunk completes no event"
        );
        let events = decoder
            .push(b"\ndata: world\n\n")
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("valid frames");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }

    #[test]
    fn handles_crlf_and_comments_and_id() {
        let mut decoder = SseDecoder::new();
        let events = decoder
            .push(b": comment\r\nid: 42\r\ndata: done\r\n\r\n")
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("valid frame");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].data, "done");
    }

    #[test]
    fn passes_done_sentinel_through_verbatim() {
        let mut decoder = SseDecoder::new();
        let events = decoder
            .push(b"data: [DONE]\n\n")
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("valid frame");
        assert_eq!(events[0].data, "[DONE]");
    }

    #[test]
    fn ignores_blocks_without_a_field() {
        let mut decoder = SseDecoder::new();
        assert!(
            decoder.push(b": just a comment\n\n").is_empty(),
            "a comment block is not an event"
        );
    }

    #[test]
    fn frame_limit_accepts_exact_boundary_with_split_delimiters() {
        for delimiter in [b"\n\n".as_slice(), b"\r\n\r\n".as_slice()] {
            let mut decoder = SseDecoder::default();
            let mut frame = b"data: ".to_vec();
            frame.resize(MAX_FRAME_BYTES, b'x');
            assert!(decoder.push(&frame).is_empty(), "no delimiter yet");
            for &byte in &delimiter[..delimiter.len() - 1] {
                assert!(
                    decoder.push(&[byte]).is_empty(),
                    "delimiter is still incomplete"
                );
            }
            let events = decoder
                .push(&delimiter[delimiter.len() - 1..])
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .expect("event");
            assert_eq!(events[0].data.len(), MAX_FRAME_BYTES - 6);
            decoder.finish().expect("complete");
        }
    }

    #[test]
    fn oversized_frames_fail_with_or_without_delimiters_and_stay_failed() {
        for suffix in [b"".as_slice(), b"\n\n".as_slice(), b"\r\n\r\n".as_slice()] {
            let mut decoder = SseDecoder::new();
            let mut frame = vec![b'x'; MAX_FRAME_BYTES + 1];
            frame.extend_from_slice(suffix);
            let mut batch = decoder.push(&frame);
            assert_eq!(batch.len(), 1);
            let error = batch.pop().expect("one result").expect_err("oversized");
            assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Decode));
            assert!(decoder.buffer.len() <= MAX_FRAME_BYTES + 3);
            assert!(
                decoder.push(b"data: recovery\n\n").is_empty(),
                "failed decoder is fused"
            );
            assert!(decoder.finish().is_err());
        }
    }

    #[test]
    fn completed_events_before_oversized_frame_are_chunk_invariant() {
        let valid = b"data: preserved\n\n";
        let mut input = valid.to_vec();
        input.resize(input.len() + MAX_FRAME_BYTES + 1, b'x');
        input.extend_from_slice(b"\n\ndata: must not be emitted\n\n");

        let mut outcomes = Vec::new();
        for split in [0, valid.len()] {
            let mut decoder = SseDecoder::new();
            let mut batch = decoder.push(&input[..split]);
            batch.extend(decoder.push(&input[split..]));
            let summary: Vec<_> = batch
                .into_iter()
                .map(|item| {
                    item.map(|event| event.data)
                        .map_err(|error| (error.provider_kind(), error.to_string()))
                })
                .collect();
            assert_eq!(summary.len(), 2, "one event, then one terminal error");
            assert_eq!(summary[0].as_deref(), Ok("preserved"));
            let (kind, message) = summary[1].as_ref().expect_err("terminal error");
            assert_eq!(*kind, Some(ProviderErrorKind::Decode));
            assert!(message.contains("8 MiB"));
            assert!(
                decoder.push(b"data: ignored\n\n").is_empty(),
                "fused after error"
            );
            assert!(decoder.finish().is_err());
            outcomes.push(summary);
        }
        assert_eq!(outcomes[0], outcomes[1], "same order, text and error");
    }

    #[test]
    fn finish_rejects_an_oversized_unterminated_delimiter_prefix() {
        let mut decoder = SseDecoder::new();
        assert!(
            decoder.push(&vec![b'x'; MAX_FRAME_BYTES]).is_empty(),
            "at limit"
        );
        assert!(decoder.push(b"\r\n\r").is_empty(), "possible delimiter");
        assert!(decoder.finish().is_err(), "EOF makes the prefix frame data");
    }

    #[test]
    fn aggregate_chunk_size_is_not_a_frame_limit() {
        let frame = format!("data: {}\n\n", "x".repeat(MAX_FRAME_BYTES / 2));
        let input = frame.repeat(3);
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder
                .push(input.as_bytes())
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .expect("small frames")
                .len(),
            3
        );
        decoder.finish().expect("complete");
    }

    /// A tiny deterministic generator so the parser fuzz test reproduces exactly
    /// (FR-TEST-3).
    struct Fuzz(u64);

    impl Fuzz {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 16
        }

        fn byte(&mut self) -> u8 {
            u8::try_from(self.next() % 256).expect("byte")
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % bound.max(1) as u64).expect("fits")
        }
    }

    fn decode_in_chunks(stream: &[u8], chunks: &[usize]) -> Vec<SseEvent> {
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        let mut offset = 0;
        for size in chunks {
            let end = (offset + size).min(stream.len());
            events.extend(decoder.push(&stream[offset..end]));
            offset = end;
            if offset == stream.len() {
                break;
            }
        }
        if offset < stream.len() {
            events.extend(decoder.push(&stream[offset..]));
        }
        events
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("valid frames")
    }

    #[test]
    fn every_chunk_split_decodes_the_same_events() {
        let stream = b"event: a\ndata: one\n\ndata: two\r\n\r\nretry: 100\ndata: three\n\n";
        let whole = decode_in_chunks(stream, &[stream.len()]);
        let byte_by_byte: Vec<usize> = core::iter::repeat_n(1, stream.len()).collect();
        assert_eq!(whole, decode_in_chunks(stream, &byte_by_byte));

        let mut fuzz = Fuzz(99);
        for _ in 0..128 {
            let chunks: Vec<usize> = (0..=fuzz.below(12)).map(|_| 1 + fuzz.below(6)).collect();
            assert_eq!(whole, decode_in_chunks(stream, &chunks));
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut fuzz = Fuzz(1);
        for _ in 0..512 {
            let len = 1 + fuzz.below(64);
            let bytes: Vec<u8> = (0..len).map(|_| fuzz.byte()).collect();
            let mut decoder = SseDecoder::new();
            assert!(
                decoder.push(&bytes).iter().all(Result::is_ok),
                "bounded input"
            );
            // Whatever the arbitrary prefix left buffered, a well-formed event
            // appended afterwards still decodes: the parser never corrupts state.
            let events = decoder
                .push(b"\n\ndata: sentinel\n\n")
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .expect("bounded frames");
            assert!(
                events.iter().any(|event| event.data.contains("sentinel")),
                "a valid event after arbitrary bytes must decode"
            );
        }
    }
}
