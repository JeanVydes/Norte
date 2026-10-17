#![no_main]

use libfuzzer_sys::fuzz_target;
use rustychain_core::{ModelId, ModelRef, ProviderId, WireFamily};
use rustychain_gemini::generate::FuzzGenerate;
use rustychain_gemini::{GeminiContinuity, WIRE_GENERATE};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut decoder = FuzzGenerate::new(continuity());
    for line in text.lines().take(1024) {
        let _ = decoder.decode(line);
    }
});

fn continuity() -> GeminiContinuity {
    let model = ModelRef::new(
        ProviderId::new("google").expect("provider"),
        ModelId::new("gemini-2.5-pro").expect("model"),
    );
    let family = WireFamily::new(WIRE_GENERATE).expect("family");
    GeminiContinuity::new(family, model)
}
