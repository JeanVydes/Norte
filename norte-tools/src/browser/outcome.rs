use super::output::{ContentOrigin, InteractiveElement};

/// What one page action produced before the common readback.
#[derive(Default)]
pub(super) struct PageOutcome {
    pub(super) message: String,
    pub(super) elements: Vec<InteractiveElement>,
    pub(super) elements_matched: usize,
    pub(super) elements_offset: usize,
    pub(super) dom_snapshot: Option<String>,
    pub(super) page_text: Option<String>,
    pub(super) page_text_origin: Option<ContentOrigin>,
    pub(super) screenshot: Option<String>,
}
