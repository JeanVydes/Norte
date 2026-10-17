#![no_main]

use libfuzzer_sys::fuzz_target;
use rustychain_core::{ModelId, ModelRef, ProviderId, WireFamily};
use rustychain_openai::responses::FuzzResponses;
use rustychain_openai::{OpenAiContinuity, WIRE_RESPONSES};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut decoder = FuzzResponses::new(continuity());
    for line in text.lines().take(1024) {
        let _ = decoder.decode(line);
    }
});

fn continuity() -> OpenAiContinuity {
    let model = ModelRef::new(
        ProviderId::new("openai").expect("provider"),
        ModelId::new("gpt-5").expect("model"),
    );
    let family = WireFamily::new(WIRE_RESPONSES).expect("family");
    OpenAiContinuity::new(family, model)
}
