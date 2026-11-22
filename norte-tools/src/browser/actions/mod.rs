mod capture;
mod input;
mod navigation;

pub(in crate::browser) use capture::{current_elements, extract_dom, read_page, screenshot};
pub(in crate::browser) use input::{press_key, scroll, target_selector, type_text};
pub(in crate::browser) use navigation::{click, navigate, wait};
