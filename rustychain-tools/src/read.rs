//! The read executor: bounded, selectable reads of a text file
//! (FR-TOOL-READ-1 .. FR-TOOL-READ-5).
//!
//! A read is a projection of a file, not the file. The request says which lines
//! matter — a range, the tail, or the lines a pattern selects with context
//! around them — and the result says what it showed and where to continue, so a
//! file larger than one tool result is paged rather than refused. Nothing is
//! taken on trust from metadata: the bytes are read under a hard cap and
//! classified, so a file that grows between a check and the read cannot slip
//! past it, and a binary file is reported instead of being sprayed into the
//! model's context.

use std::path::PathBuf;
use std::sync::Arc;

use regex::{Regex, RegexBuilder};
use rustychain_core::{
    Digest, Error, FnExecutor, Part, Result, ToolDeclaration, ToolMetadata, ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fs::read_bytes;

/// Columns a line number takes in the rendered result.
const NUMBER_WIDTH: usize = 5;
/// Bytes a rendered line costs on top of its text: the number, the separator
/// and the newline.
const LINE_OVERHEAD: usize = NUMBER_WIDTH + 3;

/// An inclusive, 1-based range of lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    /// The first line to read, 1-based.
    pub start: usize,
    /// The last line to read, 1-based and inclusive. Omitted reads to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
}

/// What to read out of one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    /// The file to read.
    #[schemars(example = &"src/lib.rs")]
    pub path: PathBuf,
    /// A range of lines. Omitted reads from the first line, for as many lines
    /// as one result may show.
    #[schemars(
        description = "A range of lines. Omit it to read from the first line; the host decides how many lines one result may show.",
        example = serde_json::json!({"start": 1, "end": 40})
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
    /// How many lines to take from the end of the file. Conflicts with `lines`
    /// and with `pattern`.
    #[schemars(
        description = "How many lines to take from the end of the file. Cannot be combined with a line range or a pattern."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    /// A regular expression, applied line by line. Only the lines it selects
    /// are shown; without `lines`, the whole file is searched.
    #[schemars(
        description = "A regular expression applied line by line. Only the lines it selects are shown; without a line range the whole file is searched.",
        example = &"fn main"
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// How many lines to show around each selected line. Needs `pattern`.
    #[schemars(
        description = "How many lines to show around each selected line. Only with a pattern."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    /// Select the lines that do *not* match the pattern instead. Needs
    /// `pattern`.
    #[schemars(
        description = "Select the lines that do not match the pattern instead. Only with a pattern."
    )]
    #[serde(default)]
    pub invert: bool,
    /// Match without regard to case. Needs `pattern`.
    #[schemars(description = "Match without regard to case. Only with a pattern.")]
    #[serde(default)]
    pub ignore_case: bool,
    /// Report how many lines the pattern selects, without showing them. Needs
    /// `pattern`.
    #[schemars(
        description = "Report how many lines the pattern selects instead of showing them. Only with a pattern."
    )]
    #[serde(default)]
    pub count_only: bool,
    /// How many selected lines to take at most. Needs `pattern`. Defaults to
    /// [`ReadLimits::max_matches`].
    #[schemars(
        description = "How many selected lines to show at most. Only with a pattern; the host caps it."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_matches: Option<usize>,
}

impl ReadRequest {
    /// The first argument that means something only alongside a pattern.
    fn needs_pattern(&self) -> Option<&'static str> {
        if self.invert {
            return Some("invert");
        }
        if self.ignore_case {
            return Some("ignore_case");
        }
        if self.count_only {
            return Some("count_only");
        }
        if self.context.is_some() {
            return Some("context");
        }
        if self.max_matches.is_some() {
            return Some("max_matches");
        }
        None
    }
}

/// What kind of content a file turned out to hold (FR-TOOL-READ-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TextKind {
    /// Valid UTF-8.
    Utf8,
    /// Not valid UTF-8 and not binary: shown, with replacement characters for
    /// the bytes that are not text.
    Lossy,
    /// Binary content, which is reported rather than shown.
    Binary,
}

/// One line of the result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadLine {
    /// 1-based line number in the file.
    pub number: usize,
    /// The text of the line, without its terminator and clipped to a readable
    /// width.
    pub text: String,
    /// Whether the request selected this line, as opposed to showing it as
    /// context around one that was selected.
    pub matched: bool,
    /// The capture groups of the first match on the line, from group 1 on; a
    /// group that did not participate is `None`. Empty when the line did not
    /// match the pattern, or the pattern has no groups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Option<String>>,
    /// The 1-based column the match starts at, counted in characters. Present
    /// only on a line the pattern matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    /// Whether the text was clipped because the line is wider than
    /// [`ReadLimits::max_line_width`].
    pub truncated: bool,
}

/// What one read did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadOutput {
    pub path: PathBuf,
    pub text_kind: TextKind,
    /// Bytes in the file.
    pub bytes: usize,
    /// Lines in the file, terminators not counted.
    pub total_lines: usize,
    /// How many lines the request selected, before the output caps: the lines
    /// of the range, or the lines the pattern matched.
    pub selected: usize,
    /// Whether the request asked for the count instead of the lines.
    pub count_only: bool,
    /// The lines shown, in file order, context included.
    pub lines: Vec<ReadLine>,
    /// How many lines [`Self::lines`] holds.
    pub shown: usize,
    /// Whether part of the selection was left out of this result.
    pub truncated: bool,
    /// The line to start the next read at, 1-based, when the selection was left
    /// out. Pass it as `lines.start`, with the same `pattern` if there was one,
    /// to continue where this result stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start: Option<usize>,
    pub content_digest: Digest,
    pub request_digest: Digest,
}

/// Bounds for the read executor (FR-TOOL-FILE-1, FR-TOOL-READ-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadLimits {
    /// Refuse to read a file larger than this.
    pub max_file_bytes: usize,
    /// Cap on the bytes of one rendered result.
    pub max_output_bytes: usize,
    /// Cap on the lines one result shows. Zero is raised to one, so a read
    /// always makes progress.
    pub max_lines: usize,
    /// Cap on the lines a pattern selects per result. Zero is raised to one.
    pub max_matches: usize,
    /// Width a line is clipped to. Zero is raised to one.
    pub max_line_width: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 16 * 1024 * 1024,
            max_output_bytes: 64 * 1024,
            max_lines: 2000,
            max_matches: 200,
            max_line_width: 500,
        }
    }
}

/// Reads a text file, whole or selected (FR-TOOL-READ-1).
///
/// The path comes from the request: what may be read is the host's policy
/// (FR-POL-1), not the executor's business.
#[rustychain_core::tool(
    name = "read",
    description = "Read a text file. On its own it reads from the first line; set lines to a range, tail to the last lines, or pattern to the lines a regular expression selects with context around them. The result reports how many lines the file has, whether it was cut short and the line to continue at, so a big file is paged instead of lost. A binary file fails the call instead of being shown."
)]
pub struct ReadTool {
    limits: ReadLimits,
}

impl ReadTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: ReadLimits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: ReadLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<ReadRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    fn run(&self, request: &ReadRequest) -> Result<ToolOutput> {
        validate(request)?;
        let pattern = match &request.pattern {
            Some(pattern) => Some(compile(pattern, request.ignore_case)?),
            None => None,
        };
        let bytes = read_bytes(&request.path, self.limits.max_file_bytes)?;
        let request_digest = Digest::of_json(&serde_json::to_value(request)?);
        let output = self.inspect(request, pattern.as_ref(), &bytes, request_digest);
        render(&output)
    }

    /// Turns the bytes on disk into the result, without deciding how to render
    /// it (FR-TOOL-READ-3).
    fn inspect(
        &self,
        request: &ReadRequest,
        pattern: Option<&Regex>,
        bytes: &[u8],
        request_digest: Digest,
    ) -> ReadOutput {
        let text_kind = classify(bytes);
        let text = String::from_utf8_lossy(bytes);
        let source: Vec<&str> = text.lines().collect();
        let mut output = ReadOutput {
            path: request.path.clone(),
            text_kind,
            bytes: bytes.len(),
            total_lines: source.len(),
            selected: 0,
            count_only: request.count_only,
            lines: Vec::new(),
            shown: 0,
            truncated: false,
            next_start: None,
            content_digest: Digest::of(bytes),
            request_digest,
        };
        if text_kind == TextKind::Binary {
            return output;
        }

        let selection = select(&source, request, pattern, &self.limits);
        output.selected = selection.selected;
        if request.count_only {
            return output;
        }

        let emitted = emit(&source, &selection.rows, pattern, &self.limits);
        output.truncated = selection.capped || emitted.truncated;
        output.next_start = emitted
            .lines
            .last()
            .filter(|_| output.truncated)
            .map(|line| line.number + 1);
        output.shown = emitted.lines.len();
        output.lines = emitted.lines;
        output
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<ReadRequest, ToolOutput> for ReadTool {
    async fn call(&self, request: ReadRequest) -> Result<ToolOutput> {
        self.run(&request)
    }
}

/// Convenience: build the suite's file reader as an erased function.
#[must_use]
pub fn read_tool() -> Arc<dyn rustychain_core::AnyFunction> {
    Arc::new(ReadTool::new().into_declaration())
}

/// One line the request selected, by 0-based index in the file.
#[derive(Clone, Copy, Debug)]
struct Row {
    index: usize,
    /// Whether the request selected it, as opposed to showing it as context.
    matched: bool,
}

/// Which lines a request selects, in file order.
#[derive(Debug, Default)]
struct Selection {
    /// The lines to show, capped by `max_matches` but not yet by the output.
    rows: Vec<Row>,
    /// How many lines the request selects across the whole window, cap or not.
    selected: usize,
    /// Whether lines were left out of `rows` for exceeding `max_matches`.
    capped: bool,
}

/// The lines actually rendered, under the output caps.
struct Emitted {
    lines: Vec<ReadLine>,
    /// Whether lines were left out of the result.
    truncated: bool,
}

/// Argument-level checks, run before the file is touched (FR-TOOL-READ-3).
fn validate(request: &ReadRequest) -> Result<()> {
    if request.path.as_os_str().is_empty() {
        return Err(Error::InvalidInput("read: a path is required".to_owned()));
    }
    if let Some(lines) = &request.lines {
        if lines.start == 0 {
            return Err(Error::InvalidInput(
                "read: lines.start is 1-based".to_owned(),
            ));
        }
        if lines.end.is_some_and(|end| end < lines.start) {
            return Err(Error::InvalidInput(format!(
                "read: lines.end comes before lines.start ({})",
                lines.start
            )));
        }
    }
    if request.tail == Some(0) {
        return Err(Error::InvalidInput(
            "read: tail must be at least 1 line".to_owned(),
        ));
    }
    if request.tail.is_some() && request.lines.is_some() {
        return Err(Error::InvalidInput(
            "read: use lines or tail, not both".to_owned(),
        ));
    }
    if request.tail.is_some() && request.pattern.is_some() {
        return Err(Error::InvalidInput(
            "read: use pattern or tail, not both".to_owned(),
        ));
    }
    match &request.pattern {
        Some(pattern) if pattern.is_empty() => Err(Error::InvalidInput(
            "read: the pattern is empty, so it would match every line".to_owned(),
        )),
        Some(_) => Ok(()),
        None => match request.needs_pattern() {
            Some(name) => Err(Error::InvalidInput(format!(
                "read: {name} needs a pattern to work on"
            ))),
            None => Ok(()),
        },
    }
}

/// The lines a request allows, 0-based and inclusive. `None` for an empty file.
fn window(total: usize, request: &ReadRequest) -> Option<(usize, usize)> {
    let last = total.checked_sub(1)?;
    if let Some(lines) = &request.lines {
        if lines.start > total {
            return None;
        }
        let first = lines.start - 1;
        let end = lines
            .end
            .map_or(last, |end| end.saturating_sub(1).min(last));
        return Some((first, end.max(first)));
    }
    if let Some(tail) = request.tail {
        return Some((last.saturating_sub(tail.saturating_sub(1)), last));
    }
    Some((0, last))
}

/// Which lines the request selects (FR-TOOL-READ-5).
fn select(
    source: &[&str],
    request: &ReadRequest,
    pattern: Option<&Regex>,
    limits: &ReadLimits,
) -> Selection {
    let Some((first, last)) = window(source.len(), request) else {
        return Selection::default();
    };
    let Some(pattern) = pattern else {
        return plain_selection(first, last, limits.max_lines.max(1));
    };

    let cap = request.max_matches.unwrap_or(limits.max_matches).max(1);
    let mut hits = Vec::new();
    let mut selected = 0usize;
    for (offset, line) in source[first..=last].iter().enumerate() {
        // An inverted request selects what the pattern leaves alone.
        if pattern.is_match(line) != request.invert {
            selected += 1;
            let index = first + offset;
            if hits.len() < cap {
                hits.push(index);
            }
        }
    }
    Selection {
        rows: expand(&hits, request.context.unwrap_or(0), first, last),
        selected,
        capped: selected > hits.len(),
    }
}

/// A read with no pattern: a contiguous run of lines, which the output caps may
/// still cut short.
fn plain_selection(first: usize, last: usize, max_lines: usize) -> Selection {
    let width = last - first + 1;
    let take = max_lines.min(width);
    Selection {
        rows: (first..first + take)
            .map(|index| Row {
                index,
                matched: true,
            })
            .collect(),
        selected: width,
        capped: width > take,
    }
}

/// The rows the selected lines need: each one, plus `context` lines on either
/// side, kept inside the window. The lines are visited in order, so the rows
/// are in order too and each one appears once.
fn expand(hits: &[usize], context: usize, first: usize, last: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut cursor = first;
    for &hit in hits {
        let end = hit.saturating_add(context).min(last);
        if cursor <= end {
            let start = cursor.max(hit.saturating_sub(context));
            rows.extend((start..=end).map(|index| Row {
                index,
                matched: false,
            }));
            cursor = end + 1;
        }
    }
    for &hit in hits {
        if let Ok(position) = rows.binary_search_by_key(&hit, |row| row.index) {
            rows[position].matched = true;
        }
    }
    rows
}

/// Renders the rows under the output caps, so one result stays readable whatever
/// the file holds. The first line is always rendered, which is what makes
/// [`ReadOutput::next_start`] a step forward rather than a loop.
fn emit(source: &[&str], rows: &[Row], pattern: Option<&Regex>, limits: &ReadLimits) -> Emitted {
    let max_lines = limits.max_lines.max(1);
    let max_line_width = limits.max_line_width.max(1);
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for row in rows {
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }
        let (text, clipped) = clip(source[row.index], max_line_width);
        let cost = text.len() + LINE_OVERHEAD;
        if !lines.is_empty() && bytes + cost > limits.max_output_bytes {
            truncated = true;
            break;
        }
        bytes += cost;
        let (groups, column) = match pattern {
            Some(pattern) if row.matched => capture(pattern, source[row.index]),
            _ => (Vec::new(), None),
        };
        lines.push(ReadLine {
            number: row.index + 1,
            text,
            matched: row.matched,
            groups,
            column,
            truncated: clipped,
        });
    }
    Emitted { lines, truncated }
}

/// What kind of content the bytes are. Only the bytes are trusted: UTF-8 is
/// text, and anything else that holds a NUL is binary even if it happens to be
/// mostly text (FR-TOOL-READ-2).
fn classify(bytes: &[u8]) -> TextKind {
    if std::str::from_utf8(bytes).is_ok() {
        return TextKind::Utf8;
    }
    if bytes.contains(&0) {
        return TextKind::Binary;
    }
    TextKind::Lossy
}

fn compile(pattern: &str, ignore_case: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| Error::InvalidInput(format!("read: invalid pattern `{pattern}`: {error}")))
}

/// The capture groups of the first match on a line, from group 1 on, and the
/// column it starts at.
fn capture(pattern: &Regex, line: &str) -> (Vec<Option<String>>, Option<usize>) {
    let Some(captures) = pattern.captures(line) else {
        return (Vec::new(), None);
    };
    let column = captures
        .get(0)
        .map_or(1, |whole| line[..whole.start()].chars().count() + 1);
    let groups = captures
        .iter()
        .skip(1)
        .map(|group| group.map(|found| found.as_str().to_owned()))
        .collect();
    (groups, Some(column))
}

/// Clips a line to `width` characters, saying whether it had to.
fn clip(line: &str, width: usize) -> (String, bool) {
    match line.char_indices().nth(width) {
        Some((end, _)) => (format!("{}…", &line[..end]), true),
        None => (line.to_owned(), false),
    }
}

fn render(output: &ReadOutput) -> Result<ToolOutput> {
    audit(output);
    Ok(ToolOutput {
        parts: vec![Part::text(describe(output))],
        structured: Some(serde_json::to_value(output)?),
        is_error: output.text_kind == TextKind::Binary,
    })
}

fn describe(output: &ReadOutput) -> String {
    use std::fmt::Write as _;

    let mut text = format!(
        "read {} ({}, {} line(s), {} bytes)",
        output.path.display(),
        kind_text(output.text_kind),
        output.total_lines,
        output.bytes
    );
    if output.text_kind == TextKind::Binary {
        let _ = writeln!(text, ": binary content is not shown");
        return text;
    }
    let _ = writeln!(text);
    if output.text_kind == TextKind::Lossy {
        let _ = writeln!(
            text,
            "note: not valid UTF-8; bytes that are not text are shown as U+FFFD"
        );
    }
    if output.count_only {
        let _ = writeln!(
            text,
            "{} of {} line(s) selected",
            output.selected, output.total_lines
        );
        return text;
    }
    if output.lines.is_empty() {
        let _ = writeln!(text, "no line selected");
        return text;
    }

    for (position, line) in output.lines.iter().enumerate() {
        if let Some(previous) = position.checked_sub(1).and_then(|at| output.lines.get(at)) {
            if line.number > previous.number + 1 {
                let _ = writeln!(text, "--");
            }
        }
        let marker = if line.matched { ':' } else { '-' };
        let _ = writeln!(
            text,
            "{:>width$}{marker} {}",
            line.number,
            line.text,
            width = NUMBER_WIDTH
        );
        if let Some(groups) = render_groups(&line.groups) {
            let _ = writeln!(text, "       groups: {groups}");
        }
    }
    if let Some(next) = output.next_start {
        let _ = writeln!(
            text,
            "... cut short after {} line(s); continue with lines.start = {next}",
            output.shown
        );
    }
    text
}

/// The captured groups of a matched line, when the pattern had any.
fn render_groups(groups: &[Option<String>]) -> Option<String> {
    if groups.is_empty() || groups.iter().all(Option::is_none) {
        return None;
    }
    let rendered: Vec<String> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| match group {
            Some(value) => format!("{}={value}", index + 1),
            None => format!("{}=-", index + 1),
        })
        .collect();
    Some(rendered.join(" "))
}

fn kind_text(kind: TextKind) -> &'static str {
    match kind {
        TextKind::Utf8 => "utf8",
        TextKind::Lossy => "lossy",
        TextKind::Binary => "binary",
    }
}

fn audit(output: &ReadOutput) {
    tracing::info!(
        target: "rustychain.audit",
        tool = "read",
        path = %output.path.display(),
        text_kind = ?output.text_kind,
        bytes = output.bytes,
        total_lines = output.total_lines,
        selected = output.selected,
        shown = output.shown,
        truncated = output.truncated,
        content_digest = %output.content_digest,
        request_digest = %output.request_digest,
        "file read"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ticket = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "rustychain-read-{name}-{}-{ticket}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("scratch");
        directory
    }

    fn seed(directory: &std::path::Path, name: &str, content: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, content).expect("seed");
        path
    }

    /// A read with nothing selected: the whole file, from its first line.
    fn plain(path: &std::path::Path) -> ReadRequest {
        ReadRequest {
            path: path.to_path_buf(),
            lines: None,
            tail: None,
            pattern: None,
            context: None,
            invert: false,
            ignore_case: false,
            count_only: false,
            max_matches: None,
        }
    }

    fn matching(path: &std::path::Path, pattern: &str) -> ReadRequest {
        ReadRequest {
            pattern: Some(pattern.to_owned()),
            ..plain(path)
        }
    }

    async fn read(tool: ReadTool, request: ReadRequest) -> ReadOutput {
        let output = tool.call(request).await.expect("the read itself works");
        assert!(!output.is_error, "the read did not fail");
        serde_json::from_value(output.structured.expect("structured output"))
            .expect("the structured output is the documented shape")
    }

    async fn failure(tool: ReadTool, request: ReadRequest) -> Error {
        tool.call(request).await.expect_err("the read fails")
    }

    async fn rendered(tool: ReadTool, request: ReadRequest) -> String {
        let output = tool.call(request).await.expect("the read works");
        output
            .parts
            .first()
            .and_then(Part::as_text)
            .expect("the result carries text")
            .to_owned()
    }

    fn numbers(output: &ReadOutput) -> Vec<usize> {
        output.lines.iter().map(|line| line.number).collect()
    }

    #[tokio::test]
    async fn a_plain_read_shows_the_file_from_its_first_line() {
        let directory = scratch("plain");
        let path = seed(&directory, "notes.txt", "one\ntwo\nthree\n");

        let output = read(ReadTool::new(), plain(&path)).await;

        assert_eq!(output.text_kind, TextKind::Utf8);
        assert_eq!(output.total_lines, 3);
        assert_eq!(output.bytes, 14);
        assert_eq!(output.selected, 3);
        assert_eq!(output.shown, 3);
        assert!(!output.truncated);
        assert_eq!(output.next_start, None);
        assert_eq!(numbers(&output), vec![1, 2, 3]);
        assert_eq!(output.lines[1].text, "two");
        assert!(output.lines.iter().all(|line| line.matched));
        assert_eq!(output.content_digest, Digest::of(b"one\ntwo\nthree\n"));
    }

    #[tokio::test]
    async fn a_long_file_is_paged_instead_of_refused() {
        let directory = scratch("paged");
        let path = seed(&directory, "long.txt", "1\n2\n3\n4\n");
        let tool = ReadTool::new().with_limits(ReadLimits {
            max_lines: 2,
            ..ReadLimits::default()
        });

        let first = read(tool.clone(), plain(&path)).await;
        assert!(first.truncated);
        assert_eq!(first.selected, 4, "the whole window is counted");
        assert_eq!(numbers(&first), vec![1, 2]);
        assert_eq!(first.next_start, Some(3));

        let mut second = plain(&path);
        second.lines = Some(LineRange {
            start: first.next_start.expect("a continuation"),
            end: None,
        });
        let second = read(tool, second).await;
        assert_eq!(numbers(&second), vec![3, 4]);
        assert!(!second.truncated);
        assert_eq!(second.next_start, None);
    }

    #[tokio::test]
    async fn a_line_range_selects_only_those_lines() {
        let directory = scratch("range");
        let path = seed(&directory, "five.txt", "1\n2\n3\n4\n5\n");

        let mut request = plain(&path);
        request.lines = Some(LineRange {
            start: 2,
            end: Some(3),
        });
        let output = read(ReadTool::new(), request).await;
        assert_eq!(numbers(&output), vec![2, 3]);
        assert_eq!(output.selected, 2);
        assert!(!output.truncated, "the range is exactly what was asked for");

        let mut open_ended = plain(&path);
        open_ended.lines = Some(LineRange {
            start: 3,
            end: None,
        });
        let output = read(ReadTool::new(), open_ended).await;
        assert_eq!(numbers(&output), vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn a_range_past_the_end_of_the_file_selects_nothing() {
        let directory = scratch("past");
        let path = seed(&directory, "short.txt", "only\n");

        let mut request = plain(&path);
        request.lines = Some(LineRange {
            start: 9,
            end: None,
        });
        let output = read(ReadTool::new(), request).await;
        assert_eq!(output.selected, 0, "the file is shorter than that");
        assert!(output.lines.is_empty());
        assert_eq!(output.next_start, None);
        assert_eq!(
            output.total_lines, 1,
            "how long the file is still comes back"
        );
    }

    #[tokio::test]
    async fn a_tail_read_shows_the_end_of_the_file() {
        let directory = scratch("tail");
        let path = seed(&directory, "five.txt", "1\n2\n3\n4\n5\n");

        let mut request = plain(&path);
        request.tail = Some(2);
        let output = read(ReadTool::new(), request).await;
        assert_eq!(numbers(&output), vec![4, 5]);
        assert_eq!(output.selected, 2);
        assert_eq!(output.next_start, None, "there is nothing after the tail");

        let mut too_many = plain(&path);
        too_many.tail = Some(99);
        let output = read(ReadTool::new(), too_many).await;
        assert_eq!(numbers(&output), vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn a_pattern_selects_the_lines_it_matches() {
        let directory = scratch("pattern");
        let path = seed(&directory, "log.txt", "alpha 1\nbeta 2\nalpha 3\n");

        let output = read(ReadTool::new(), matching(&path, r"alpha (\d+)")).await;

        assert_eq!(numbers(&output), vec![1, 3]);
        assert_eq!(output.selected, 2);
        assert!(output.lines.iter().all(|line| line.matched));
        assert_eq!(output.lines[0].groups, vec![Some("1".to_owned())]);
        assert_eq!(output.lines[0].column, Some(1));
        assert_eq!(
            output.lines[1].column,
            Some(1),
            "the second match starts at the line's first character too"
        );
        assert_eq!(output.lines[1].groups, vec![Some("3".to_owned())]);
    }

    #[tokio::test]
    async fn a_capture_that_did_not_participate_is_absent() {
        let directory = scratch("groups");
        let path = seed(&directory, "log.txt", "value=42\nvalue=\n");

        let output = read(ReadTool::new(), matching(&path, r"value=(\d+)?")).await;

        assert_eq!(numbers(&output), vec![1, 2]);
        assert_eq!(output.lines[0].groups, vec![Some("42".to_owned())]);
        assert_eq!(output.lines[1].groups, vec![None]);
    }

    #[tokio::test]
    async fn a_column_counts_characters_not_bytes() {
        let directory = scratch("column");
        let path = seed(&directory, "wide.txt", "→→needle\n");

        let output = read(ReadTool::new(), matching(&path, "needle")).await;

        assert_eq!(
            output.lines[0].column,
            Some(3),
            "two characters before the match, whatever they cost in bytes"
        );
    }

    #[tokio::test]
    async fn ignore_case_matches_whatever_the_case_is() {
        let directory = scratch("case");
        let path = seed(&directory, "log.txt", "Alpha\nalpha\nALPHA\nbeta\n");

        let mut request = matching(&path, "alpha");
        request.ignore_case = true;
        let output = read(ReadTool::new(), request).await;

        assert_eq!(numbers(&output), vec![1, 2, 3]);
        assert_eq!(output.selected, 3);
    }

    #[tokio::test]
    async fn invert_selects_what_the_pattern_leaves_alone() {
        let directory = scratch("invert");
        let path = seed(&directory, "log.txt", "alpha\nbeta\nalpha\n");

        let mut request = matching(&path, "alpha");
        request.invert = true;
        let output = read(ReadTool::new(), request).await;

        assert_eq!(numbers(&output), vec![2]);
        assert_eq!(output.selected, 1);
        assert!(
            output.lines[0].matched,
            "an inverted line is what was asked for"
        );
    }

    #[tokio::test]
    async fn context_widens_the_result_without_marking_it() {
        let directory = scratch("context");
        let path = seed(&directory, "log.txt", "1\nneedle\n3\n4\n");

        let mut request = matching(&path, "needle");
        request.context = Some(1);
        let output = read(ReadTool::new(), request).await;

        assert_eq!(numbers(&output), vec![1, 2, 3]);
        assert_eq!(
            output.selected, 1,
            "one line matched; the neighbours are context"
        );
        assert_eq!(output.shown, 3);
        let matched: Vec<usize> = output
            .lines
            .iter()
            .filter(|line| line.matched)
            .map(|line| line.number)
            .collect();
        assert_eq!(matched, vec![2]);
    }

    #[tokio::test]
    async fn a_narrow_range_narrows_the_search() {
        let directory = scratch("within");
        let path = seed(&directory, "log.txt", "needle\nx\nneedle\ny\nneedle\n");

        let mut request = matching(&path, "needle");
        request.lines = Some(LineRange {
            start: 2,
            end: Some(4),
        });
        let output = read(ReadTool::new(), request).await;

        assert_eq!(numbers(&output), vec![3], "only the match inside the range");
        assert_eq!(output.selected, 1);
    }

    #[tokio::test]
    async fn count_only_reports_the_number_without_the_lines() {
        let directory = scratch("count");
        let path = seed(&directory, "log.txt", "alpha\nbeta\nalpha\n");

        let mut request = matching(&path, "alpha");
        request.count_only = true;
        let text = rendered(ReadTool::new(), request.clone()).await;
        let output = read(ReadTool::new(), request).await;

        assert!(output.count_only);
        assert_eq!(output.selected, 2);
        assert!(output.lines.is_empty());
        assert_eq!(output.shown, 0);
        assert_eq!(output.next_start, None);
        assert!(
            text.contains("2 of 3 line(s) selected"),
            "the count is what the model reads: {text}"
        );
    }

    #[tokio::test]
    async fn max_matches_caps_the_result_and_points_at_the_rest() {
        let directory = scratch("cap");
        let path = seed(&directory, "log.txt", "hit\nhit\nhit\nhit\nhit\n");

        let mut request = matching(&path, "hit");
        request.max_matches = Some(2);
        let output = read(ReadTool::new(), request).await;

        assert_eq!(numbers(&output), vec![1, 2]);
        assert_eq!(
            output.selected, 5,
            "how many there are is still reported in full"
        );
        assert!(output.truncated);
        assert_eq!(output.next_start, Some(3));

        let mut rest = matching(&path, "hit");
        rest.max_matches = Some(2);
        rest.lines = Some(LineRange {
            start: 3,
            end: None,
        });
        let rest = read(ReadTool::new(), rest).await;
        assert_eq!(numbers(&rest), vec![3, 4]);
        assert_eq!(rest.next_start, Some(5));
    }

    #[tokio::test]
    async fn the_byte_budget_pages_through_a_file_without_skipping_a_line() {
        let directory = scratch("budget");
        let path = seed(
            &directory,
            "lines.txt",
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n",
        );
        let tool = ReadTool::new().with_limits(ReadLimits {
            max_output_bytes: 20,
            ..ReadLimits::default()
        });

        let mut start = 1usize;
        let mut seen: Vec<usize> = Vec::new();
        loop {
            let mut request = plain(&path);
            request.lines = Some(LineRange { start, end: None });
            let output = read(tool.clone(), request).await;
            assert!(
                output.lines.len() <= 1,
                "one line costs more than the whole budget"
            );
            for line in &output.lines {
                assert!(
                    !seen.contains(&line.number),
                    "line {} came back twice",
                    line.number
                );
                seen.push(line.number);
            }
            let Some(next) = output.next_start else {
                break;
            };
            assert!(next > start, "each result moves the window forward");
            start = next;
        }
        assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn a_wide_line_is_clipped_and_says_so() {
        let directory = scratch("wide");
        let path = seed(&directory, "wide.txt", &format!("{}\n", "x".repeat(50)));
        let tool = ReadTool::new().with_limits(ReadLimits {
            max_line_width: 10,
            ..ReadLimits::default()
        });

        let output = read(tool.clone(), plain(&path)).await;

        assert!(output.lines[0].truncated);
        assert_eq!(output.lines[0].text, format!("{}…", "x".repeat(10)));
        assert!(rendered(tool, plain(&path)).await.contains('…'));
    }

    #[tokio::test]
    async fn the_rendered_result_reads_like_a_grep() {
        let directory = scratch("render");
        let path = seed(&directory, "log.txt", "a\nneedle 7\nb\nc\nd\ne\nneedle 9\n");

        let mut request = matching(&path, r"needle (\d+)");
        request.context = Some(1);
        let text = rendered(ReadTool::new(), request).await;

        assert!(
            text.starts_with("read "),
            "the header names the file: {text}"
        );
        assert!(text.contains("    2: needle 7"), "{text}");
        assert!(text.contains("    7: needle 9"), "{text}");
        assert!(text.contains("    1- a"), "context is marked apart: {text}");
        assert!(text.contains("    3- b"), "{text}");
        assert!(text.contains("--"), "blocks are separated: {text}");
        assert!(text.contains("groups: 1=7"), "{text}");
        assert!(text.contains("groups: 1=9"), "{text}");
    }

    #[tokio::test]
    async fn a_binary_file_fails_instead_of_being_shown() {
        let directory = scratch("binary");
        let path = directory.join("blob.bin");
        std::fs::write(&path, [0x00, 0x01, 0xff, 0xfe]).expect("seed");

        let output = ReadTool::new()
            .call(plain(&path))
            .await
            .expect("the read itself works");

        assert!(output.is_error, "binary content is a failed call");
        let structured: ReadOutput =
            serde_json::from_value(output.structured.expect("structured")).expect("shape");
        assert_eq!(structured.text_kind, TextKind::Binary);
        assert!(structured.lines.is_empty());
        assert_eq!(structured.bytes, 4);
        let text = output.parts[0].as_text().expect("text");
        assert!(text.contains("binary"), "{text}");
    }

    #[tokio::test]
    async fn a_lossy_file_is_shown_with_replacement_characters() {
        let directory = scratch("lossy");
        let path = directory.join("mixed.txt");
        std::fs::write(&path, b"ok\n\xff bad\n").expect("seed");

        let output = read(ReadTool::new(), plain(&path)).await;

        assert_eq!(output.text_kind, TextKind::Lossy);
        assert_eq!(output.total_lines, 2);
        assert_eq!(numbers(&output), vec![1, 2]);
        assert!(
            output.lines[1].text.contains('\u{fffd}'),
            "the byte that is not text is replaced: {:?}",
            output.lines[1].text
        );
        let text = rendered(ReadTool::new(), plain(&path)).await;
        assert!(text.contains("U+FFFD"), "the reader is told: {text}");
    }

    #[tokio::test]
    async fn an_empty_file_reads_as_no_lines() {
        let directory = scratch("empty");
        let path = seed(&directory, "empty.txt", "");

        let output = read(ReadTool::new(), plain(&path)).await;

        assert_eq!(output.total_lines, 0);
        assert_eq!(output.bytes, 0);
        assert!(output.lines.is_empty());
        assert!(!output.truncated);
        assert_eq!(output.next_start, None);
        assert!(
            rendered(ReadTool::new(), plain(&path))
                .await
                .contains("no line selected")
        );
    }

    #[tokio::test]
    async fn an_empty_path_is_invalid_input() {
        let error = failure(ReadTool::new(), plain(std::path::Path::new(""))).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("path")));
    }

    #[tokio::test]
    async fn a_zero_based_start_is_rejected() {
        let mut request = plain(std::path::Path::new("ignored.txt"));
        request.lines = Some(LineRange {
            start: 0,
            end: None,
        });
        let error = failure(ReadTool::new(), request).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("1-based")));
    }

    #[tokio::test]
    async fn a_range_that_ends_before_it_starts_is_rejected() {
        let mut request = plain(std::path::Path::new("ignored.txt"));
        request.lines = Some(LineRange {
            start: 5,
            end: Some(2),
        });
        let error = failure(ReadTool::new(), request).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("lines.end")));
    }

    #[tokio::test]
    async fn a_tail_of_zero_is_rejected() {
        let mut request = plain(std::path::Path::new("ignored.txt"));
        request.tail = Some(0);
        let error = failure(ReadTool::new(), request).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("tail")));
    }

    #[tokio::test]
    async fn a_tail_conflicts_with_a_range_and_with_a_pattern() {
        let mut with_range = plain(std::path::Path::new("ignored.txt"));
        with_range.tail = Some(2);
        with_range.lines = Some(LineRange {
            start: 1,
            end: None,
        });
        let error = failure(ReadTool::new(), with_range).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("not both")));

        let mut with_pattern = matching(std::path::Path::new("ignored.txt"), "x");
        with_pattern.tail = Some(2);
        let error = failure(ReadTool::new(), with_pattern).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("not both")));
    }

    #[tokio::test]
    async fn arguments_that_need_a_pattern_say_so() {
        let path = std::path::Path::new("ignored.txt");
        let alone = [
            ReadRequest {
                invert: true,
                ..plain(path)
            },
            ReadRequest {
                ignore_case: true,
                ..plain(path)
            },
            ReadRequest {
                count_only: true,
                ..plain(path)
            },
            ReadRequest {
                context: Some(1),
                ..plain(path)
            },
            ReadRequest {
                max_matches: Some(1),
                ..plain(path)
            },
        ];
        for request in alone {
            let error = failure(ReadTool::new(), request).await;
            assert!(
                matches!(error, Error::InvalidInput(ref reason) if reason.contains("needs a pattern")),
                "every pattern-only argument is refused: {error}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_pattern_is_rejected() {
        let path = std::path::Path::new("ignored.txt");
        let error = failure(ReadTool::new(), matching(path, "")).await;
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("empty")));
    }

    #[tokio::test]
    async fn an_invalid_pattern_names_the_pattern() {
        let path = std::path::Path::new("ignored.txt");
        let error = failure(ReadTool::new(), matching(path, "(unclosed")).await;
        assert!(
            matches!(error, Error::InvalidInput(ref reason) if reason.contains("invalid pattern")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_missing_file_names_the_path() {
        let directory = scratch("missing");
        let path = directory.join("absent.txt");
        let error = failure(ReadTool::new(), plain(&path)).await;
        assert!(error.to_string().contains("absent.txt"), "{error}");
    }

    #[tokio::test]
    async fn reading_a_directory_names_the_path() {
        let directory = scratch("isdir");
        let error = failure(ReadTool::new(), plain(&directory)).await;
        assert!(
            error
                .to_string()
                .contains(directory.to_string_lossy().as_ref()),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused() {
        let directory = scratch("limit");
        let path = seed(&directory, "big.txt", "0123456789");
        let tool = ReadTool::new().with_limits(ReadLimits {
            max_file_bytes: 4,
            ..ReadLimits::default()
        });

        let error = failure(tool, plain(&path)).await;
        assert!(matches!(error, Error::LimitExceeded { limit: 4, .. }));
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&ReadTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("read").expect("name");
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(spec.description, <ReadTool as ToolMetadata>::DESCRIPTION);

        let directory = scratch("envelope");
        let path = seed(&directory, "notes.txt", "first\nsecond\nthird\n");
        let output = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": path,
                    "lines": { "start": 2, "end": 3 }
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        let text = output.parts[0].as_text().expect("text");
        assert!(text.contains("    2: second"), "{text}");
        assert!(text.contains("    3: third"), "{text}");
    }

    #[tokio::test]
    async fn unknown_arguments_are_rejected_before_the_body_runs() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&ReadTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("read").expect("name");
        let error = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": "ignored.txt",
                    "mode": "append"
                }),
            )
            .await
            .expect_err("unknown field (FR-TOOL-2)");
        assert!(matches!(error, Error::InvalidToolArguments { .. }));
    }

    #[test]
    fn the_default_limits_are_the_documented_ones() {
        let limits = ReadLimits::default();
        assert_eq!(limits.max_file_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.max_output_bytes, 64 * 1024);
        assert_eq!(limits.max_lines, 2000);
        assert_eq!(limits.max_matches, 200);
        assert_eq!(limits.max_line_width, 500);
    }
}
