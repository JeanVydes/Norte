//! The text one tool hands a model: bounded in width, with the characters a
//! terminal would act on escaped, so no line can hide or reorder the rest of a
//! result (FR-TOOL-READ-5).

use std::fmt::Write as _;

/// `width` characters of `line` from character `begin`, and whether the line
/// had more after them.
pub(crate) fn char_window(line: &str, begin: usize, width: usize) -> (String, bool) {
    let mut window = String::new();
    let mut taken = 0usize;
    let mut trailing = false;
    for (position, ch) in line.chars().enumerate() {
        if position < begin {
            continue;
        }
        if taken == width {
            trailing = true;
            break;
        }
        window.push(ch);
        taken += 1;
    }
    (window, trailing)
}

/// Clips a line to `width` characters, saying whether it had to.
pub(crate) fn clip(line: &str, width: usize) -> (String, bool) {
    let (text, trailing) = char_window(line, 0, width);
    if trailing {
        (format!("{text}…"), true)
    } else {
        (text, false)
    }
}

/// The characters a terminal would act on, escaped: a lone carriage return, an
/// escape sequence, a bidi override or a line separator inside a line could
/// otherwise hide or reorder the rest of the result. A tab is left alone — it is
/// indentation, which is text — and so is every printable character, so only a
/// line that holds a control character pays for the escape.
pub(crate) fn escape(text: &str) -> String {
    if !text
        .chars()
        .any(|ch| (ch.is_control() && ch != '\t') || is_bidi(ch))
    {
        return text.to_owned();
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\t' => escaped.push('\t'),
            '\r' => escaped.push_str("\\r"),
            '\u{1b}' => escaped.push_str("\\e"),
            ch if ch.is_control() || is_bidi(ch) => {
                let _ = write!(escaped, "\\u{{{:x}}}", u32::from(ch));
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// Whether a character reorders or hides text where a terminal renders it.
fn is_bidi(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}
