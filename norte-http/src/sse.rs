use std::time::Duration;

/// One server-sent event, already decoded from the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<Duration>,
}

/// Incremental `SSE` parser. Feed it raw chunks in any split; it returns the
/// events that completed, keeping the remainder buffered.
///
/// The parser is transport-free and pure, so every vendor mapping can be tested
/// without a socket.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a chunk and returns every event it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((index, len)) = find_separator(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..index + len).collect();
            let block = &block[..index];
            if let Some(event) = parse_block(block) {
                events.push(event);
            }
        }
        events
    }

    /// Whether the buffer holds a partial event.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

fn find_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find(buffer, b"\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
        let events = decoder.push(b"event: message_start\ndata: {\"a\":\ndata: 1}\n\n");
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
        let events = decoder.push(b"\ndata: world\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }

    #[test]
    fn handles_crlf_and_comments_and_id() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b": comment\r\nid: 42\r\ndata: done\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].data, "done");
    }

    #[test]
    fn passes_done_sentinel_through_verbatim() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: [DONE]\n\n");
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
            let _ = decoder.push(&bytes);
            // Whatever the arbitrary prefix left buffered, a well-formed event
            // appended afterwards still decodes: the parser never corrupts state.
            let events = decoder.push(b"\n\ndata: sentinel\n\n");
            assert!(
                events.iter().any(|event| event.data.contains("sentinel")),
                "a valid event after arbitrary bytes must decode"
            );
        }
    }
}
