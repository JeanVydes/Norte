//! Console rendering: live tokens, tool activity and approval prompts.

use std::error::Error;
use std::io::{self, Write};

use rustychain_core::{EngineEvent, Part, StreamDelta};

pub(crate) const RESET: &str = "\x1b[0m";
pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const DIM: &str = "\x1b[2m";
pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const MAGENTA: &str = "\x1b[35m";
pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const RED: &str = "\x1b[31m";
pub(crate) const YELLOW: &str = "\x1b[33m";

/// Writes to stdout and flushes, so a token appears the moment it arrives.
pub(crate) fn emit(text: &str) {
    print!("{text}");
    let _ = io::stdout().flush();
}

/// Prints a titled separator.
pub(crate) fn section(title: &str, colour: &str) {
    emit(&format!("\n{colour}{BOLD}── {title} ──{RESET}\n"));
}

/// Asks a yes/no question on the terminal; only `y` or `yes` approves.
pub(crate) fn confirm(question: &str) -> bool {
    emit(&format!("{YELLOW}{question} [y/N] {RESET}"));
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        Ok(_) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

/// Reports a failed branch and keeps the run going.
pub(crate) fn failure(label: &str, error: &dyn Error) -> String {
    emit(&format!("\n{RED}  ! {label} failed: {error}{RESET}\n"));
    String::new()
}

/// Renders one engine event as it happens.
pub(crate) fn event(event: &EngineEvent) {
    match event {
        EngineEvent::StreamDelta { delta, .. } => delta_line(delta),
        EngineEvent::ToolCallStarted { tool, .. } => {
            emit(&format!("\n{CYAN}▶ {tool}{RESET} {DIM}"));
        }
        EngineEvent::ToolCallFinished {
            tool,
            is_error,
            duration_ms,
            ..
        } => {
            let (colour, status) = if *is_error {
                (RED, "failed")
            } else {
                (GREEN, "ok")
            };
            emit(&format!(
                "\n{colour}  ← {tool} {status} in {duration_ms} ms{RESET}\n"
            ));
        }
        EngineEvent::PartCompleted {
            part: Part::ToolResult(result),
            ..
        } => {
            let text = result
                .parts
                .iter()
                .filter_map(Part::as_text)
                .collect::<Vec<_>>()
                .join(" ");
            emit(&format!("{DIM}  ↳ {}{RESET}\n", preview(&text, 240)));
        }
        _ => {}
    }
}

fn delta_line(delta: &StreamDelta) {
    match delta {
        StreamDelta::Text { text, .. } => emit(text),
        StreamDelta::Reasoning { text, .. } => emit(&format!("{DIM}{text}{RESET}")),
        StreamDelta::ToolArguments { arguments, .. } => emit(&format!("{DIM}{arguments}{RESET}")),
        StreamDelta::Refusal { text, .. } => emit(&format!("{RED}{text}{RESET}")),
        _ => {}
    }
}

fn preview(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        return compact;
    }
    let mut out = compact.chars().take(max).collect::<String>();
    out.push('…');
    out
}
