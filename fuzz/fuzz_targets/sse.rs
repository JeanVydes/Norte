#![no_main]

use libfuzzer_sys::fuzz_target;
use rustychain_http::sse::SseDecoder;

fuzz_target!(|data: &[u8]| {
    let mut decoder = SseDecoder::new();
    let mut chunk = data;
    while !chunk.is_empty() {
        let take = chunk.len().min(64);
        let (head, tail) = chunk.split_at(take);
        let _ = decoder.push(head);
        chunk = tail;
    }
    let _ = decoder.is_empty();
});
