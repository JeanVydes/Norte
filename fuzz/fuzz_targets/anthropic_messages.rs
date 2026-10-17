#![no_main]

use libfuzzer_sys::fuzz_target;
use rustychain_anthropic::messages::FuzzMessages;
use rustychain_anthropic::{AnthropicContinuity, WIRE_MESSAGES};
use rustychain_core::{ModelId, ModelRef, ProviderId, WireFamily};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut decoder = FuzzMessages::new(continuity());
    for line in text.lines().take(1024) {
        let _ = decoder.decode(line);
    }
});

fn continuity() -> AnthropicContinuity {
    let model = ModelRef::new(
        ProviderId::new("anthropic").expect("provider"),
        ModelId::new("claude-sonnet-4").expect("model"),
    );
    let family = WireFamily::new(WIRE_MESSAGES).expect("family");
    AnthropicContinuity::new(family, model)
}
