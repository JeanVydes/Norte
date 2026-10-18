//! The edit executor: search-and-replace over a text file, with a
//! whitespace-tolerant fallback and failures that show the lines that are
//! actually there (FR-TOOL-EDIT-1 .. FR-TOOL-EDIT-5).
//!
//! Every edit is applied to the file in memory and the file is written once, so
//! a request that fails on its third pair leaves the file byte-identical. When a
//! block does not match exactly, the executor compares whole lines with
//! whitespace runs collapsed; a block off by indentation alone therefore still
//! lands, and an edit that is too far off comes back as a failure carrying the
//! current text around the closest candidate, for the model to retry with.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::Arc;

use rustychain_core::{
    Digest, Error, FnExecutor, Part, Result, ToolDeclaration, ToolMetadata, ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fs::{read_text, write_atomic};

/// Matches scanned before a search counts as common rather than unique.
const MAX_OCCURRENCES: usize = 32;
/// Windows scored before a fallback is refused as ambiguous.
const MAX_ANCHORS: usize = 16;
/// Regions reported in a failure.
const REPORTED_SNIPPETS: usize = 5;
/// Lines shown of one reported region.
const MAX_SNIPPET_LINES: usize = 40;
/// Width of one reported line.
const MAX_LINE_WIDTH: usize = 200;

/// One search-and-replace pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditSpec {
    /// The text to find. It has to match exactly once; when nothing matches
    /// exactly, a block that differs only in whitespace still applies.
    pub search: String,
    /// What replaces it. An empty string deletes the block.
    pub replace: String,
}

/// The edits to apply to one file, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    /// The file to edit.
    #[schemars(example = &"src/main.rs")]
    pub path: PathBuf,
    /// Applied in order, each one over the result of the previous.
    #[schemars(
        example = serde_json::json!([{"search": "let x = 1;", "replace": "let x = 2;"}])
    )]
    pub edits: Vec<EditSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditCode {
    /// Every edit applied and the file was written.
    Applied,
    /// No block was close enough, or none matched at all. Nothing was written.
    NoMatch,
    /// A block matched in more than one place, or in several equally close
    /// ones. Nothing was written.
    Ambiguous,
}

/// How an edit's search was matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchKind {
    /// The block matched byte for byte, once.
    Exact,
    /// The block matched once up to whitespace, which makes the two texts
    /// identical once whitespace runs are collapsed.
    Whitespace,
    /// The block matched once with a similarity above the threshold.
    Fuzzy,
}

/// Where an edit landed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditMatch {
    /// Index of the edit in the request.
    pub edit: usize,
    pub kind: MatchKind,
    /// 1-based first line of the replaced block.
    pub start_line: usize,
    /// 1-based last line of the replaced block.
    pub end_line: usize,
    /// Similarity of the searched block to the block that was there: `1.0` for
    /// an exact match.
    pub confidence: f64,
}

/// The lines that are actually in the file, for a model that has to retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// 1-based number of the first line.
    pub start_line: usize,
    /// The lines, without terminators and clipped to a readable width.
    pub lines: Vec<String>,
    /// Lines of the region that were left out for being too long.
    pub skipped: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditFailureReason {
    /// Nothing matched exactly and no block was close enough.
    NoMatch,
    /// The block, or several equally close blocks, matched in more than one
    /// place.
    Ambiguous,
}

/// Why an edit did not apply, with the evidence to retry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditFailure {
    /// Index of the edit that failed.
    pub edit: usize,
    pub reason: EditFailureReason,
    /// How many lines the searched block has, blank lines excluded.
    pub search_lines: usize,
    /// How many places matched, or how many candidates were as good as the
    /// best one.
    pub occurrences: usize,
    /// Similarity of the best candidate to the searched block, when one was
    /// found at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// The current text around the places that mattered.
    pub candidates: Vec<Snippet>,
}

/// What one edit request did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditOutcome {
    pub code: EditCode,
    pub path: PathBuf,
    /// The edits that matched, in order. On a failure, the ones before it.
    pub matches: Vec<EditMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<EditFailure>,
    /// Whether the file was written. A failure always leaves it untouched.
    pub written: bool,
    /// Lines and bytes of the file after the call, written or not.
    pub total_lines: usize,
    pub bytes: usize,
    pub content_digest: Digest,
    pub request_digest: Digest,
}

/// Bounds for the edit executor (FR-TOOL-EDIT-3, FR-TOOL-FILE-1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditLimits {
    /// Refuse to edit a file larger than this.
    pub max_file_bytes: usize,
    /// How many lines of context a failure shows around a candidate.
    pub context_lines: usize,
    /// Similarity a block that is not an exact match needs to apply, between
    /// `0.0` and `1.0`. `None` disables the fallback and leaves only exact,
    /// unique matches.
    pub fuzzy_threshold: Option<f64>,
}

impl Default for EditLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            context_lines: 3,
            fuzzy_threshold: Some(0.95),
        }
    }
}

/// Changes a text file by search and replace (FR-TOOL-EDIT-1).
///
/// The path comes from the request: what may be edited is the host's policy
/// (FR-POL-1), not the executor's business.
#[rustychain_core::tool(
    name = "edit",
    description = "Change a text file with search and replace. Each edit's search block has to match exactly once; a block that differs only in indentation or whitespace still applies. Nothing is written unless every edit applies, and a failure reports the lines that are actually there so the next attempt can be exact."
)]
pub struct EditTool {
    limits: EditLimits,
}

impl EditTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: EditLimits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: EditLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<EditRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    fn run(&self, request: &EditRequest) -> Result<ToolOutput> {
        validate(request)?;
        let before = read_text(&request.path, self.limits.max_file_bytes)?;
        let request_digest = Digest::of_json(&serde_json::to_value(request)?);
        let outcome = match self.apply(&before, request) {
            Ok((content, matches)) => {
                write_atomic(&request.path, content.as_bytes())?;
                EditOutcome {
                    code: EditCode::Applied,
                    path: request.path.clone(),
                    matches,
                    failure: None,
                    written: true,
                    total_lines: content.lines().count(),
                    bytes: content.len(),
                    content_digest: Digest::of(content.as_bytes()),
                    request_digest,
                }
            }
            Err(Rejection { applied, failure }) => EditOutcome {
                code: code_of(failure.reason),
                path: request.path.clone(),
                matches: applied,
                failure: Some(failure),
                written: false,
                total_lines: before.lines().count(),
                bytes: before.len(),
                content_digest: Digest::of(before.as_bytes()),
                request_digest,
            },
        };
        render(&outcome)
    }

    /// Applies every edit to an in-memory copy, or reports the first one that
    /// does not apply (FR-TOOL-EDIT-1).
    fn apply(
        &self,
        before: &str,
        request: &EditRequest,
    ) -> std::result::Result<(String, Vec<EditMatch>), Rejection> {
        let mut content = before.to_owned();
        let mut matches = Vec::with_capacity(request.edits.len());
        for (index, spec) in request.edits.iter().enumerate() {
            match apply_one(&mut content, index, spec, &self.limits) {
                Ok(entry) => matches.push(entry),
                Err(failure) => {
                    return Err(Rejection {
                        applied: matches,
                        failure,
                    });
                }
            }
        }
        Ok((content, matches))
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<EditRequest, ToolOutput> for EditTool {
    async fn call(&self, request: EditRequest) -> Result<ToolOutput> {
        self.run(&request)
    }
}

/// Convenience: build the suite's file editor as an erased function.
#[must_use]
pub fn edit_tool() -> Arc<dyn rustychain_core::AnyFunction> {
    Arc::new(EditTool::new().into_declaration())
}

/// The edits that did apply before one failed, with the evidence for it.
struct Rejection {
    applied: Vec<EditMatch>,
    failure: EditFailure,
}

/// The engine's own result for one edit.
type MatchResult = std::result::Result<EditMatch, EditFailure>;

/// One window of source lines that could be the searched block.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    score: f64,
    /// 0-based line numbers, inclusive, in the file.
    first: usize,
    last: usize,
}

/// Argument-level checks, run before the file is read (FR-TOOL-EDIT-2).
fn validate(request: &EditRequest) -> Result<()> {
    if request.path.as_os_str().is_empty() {
        return Err(Error::InvalidInput("edit: a path is required".to_owned()));
    }
    if request.edits.is_empty() {
        return Err(Error::InvalidInput(
            "edit: at least one edit is required".to_owned(),
        ));
    }
    for (index, spec) in request.edits.iter().enumerate() {
        if spec.search.trim().is_empty() {
            return Err(Error::InvalidInput(format!(
                "edit: edit {index} has an empty search block"
            )));
        }
    }
    Ok(())
}

fn code_of(reason: EditFailureReason) -> EditCode {
    match reason {
        EditFailureReason::NoMatch => EditCode::NoMatch,
        EditFailureReason::Ambiguous => EditCode::Ambiguous,
    }
}

/// Applies one edit in place: exactly one match or nothing at all.
fn apply_one(
    content: &mut String,
    index: usize,
    spec: &EditSpec,
    limits: &EditLimits,
) -> MatchResult {
    let offsets = occurrences(content, &spec.search);
    let starts = line_starts(content);
    match offsets.as_slice() {
        [] => fuzzy_apply(content, index, spec, limits),
        [offset] => {
            let first = line_of(&starts, *offset);
            let last = line_of(&starts, offset + spec.search.len() - 1);
            let replacement = normalize_newlines(&spec.replace, eol_of(content));
            content.replace_range(*offset..*offset + spec.search.len(), &replacement);
            Ok(EditMatch {
                edit: index,
                kind: MatchKind::Exact,
                start_line: first + 1,
                end_line: last + 1,
                confidence: 1.0,
            })
        }
        _ => Err(ambiguous(content, &starts, index, spec, &offsets, limits)),
    }
}

/// The fallback: compare whole lines with whitespace runs collapsed, so a block
/// off by indentation alone still lands (FR-TOOL-EDIT-3).
fn fuzzy_apply(
    content: &mut String,
    index: usize,
    spec: &EditSpec,
    limits: &EditLimits,
) -> MatchResult {
    let block = normalized_block(&spec.search);
    if block.is_empty() {
        return Err(no_match(index, &spec.search, None, Vec::new()));
    }
    let source = normalized_source(content);
    let (found, windows) = candidates(&source, &block);
    let Some(best) = windows.first().copied() else {
        return Err(no_match(index, &spec.search, None, Vec::new()));
    };
    let tied: Vec<Candidate> = windows
        .iter()
        .copied()
        .filter(|window| window.score.total_cmp(&best.score).is_eq())
        .collect();
    let applies = limits
        .fuzzy_threshold
        .is_some_and(|threshold| best.score >= threshold)
        && found <= MAX_ANCHORS;
    if !applies || tied.len() > 1 {
        let reason = if applies {
            EditFailureReason::Ambiguous
        } else {
            EditFailureReason::NoMatch
        };
        let reported = if applies { &tied } else { &windows };
        let lines = line_texts(content);
        let candidates = reported
            .iter()
            .take(REPORTED_SNIPPETS)
            .map(|window| snippet(&lines, window.first, window.last, limits.context_lines))
            .collect();
        return Err(EditFailure {
            edit: index,
            reason,
            search_lines: block.len(),
            occurrences: if applies { found } else { tied.len() },
            confidence: Some(best.score),
            candidates,
        });
    }

    let replacement = normalize_newlines(&spec.replace, eol_of(content));
    splice_lines(content, best.first, best.last, &replacement);
    Ok(EditMatch {
        edit: index,
        kind: if best.score >= 1.0 {
            MatchKind::Whitespace
        } else {
            MatchKind::Fuzzy
        },
        start_line: best.first + 1,
        end_line: best.last + 1,
        confidence: best.score,
    })
}

/// Every place `search` matches, up to a cap: a match has to be unique to
/// apply, so counting further than the cap only wastes work.
fn occurrences(content: &str, search: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    for (offset, _) in content.match_indices(search) {
        offsets.push(offset);
        if offsets.len() > MAX_OCCURRENCES {
            break;
        }
    }
    offsets
}

fn ambiguous(
    content: &str,
    starts: &[usize],
    index: usize,
    spec: &EditSpec,
    offsets: &[usize],
    limits: &EditLimits,
) -> EditFailure {
    let lines = line_texts(content);
    let candidates = offsets
        .iter()
        .take(REPORTED_SNIPPETS)
        .map(|offset| {
            let first = line_of(starts, *offset);
            let last = line_of(starts, offset + spec.search.len() - 1);
            snippet(&lines, first, last, limits.context_lines)
        })
        .collect();
    EditFailure {
        edit: index,
        reason: EditFailureReason::Ambiguous,
        search_lines: normalized_block(&spec.search).len(),
        occurrences: offsets.len(),
        confidence: None,
        candidates,
    }
}

fn no_match(
    index: usize,
    search: &str,
    confidence: Option<f64>,
    candidates: Vec<Snippet>,
) -> EditFailure {
    EditFailure {
        edit: index,
        reason: EditFailureReason::NoMatch,
        search_lines: normalized_block(search).len(),
        occurrences: candidates.len(),
        confidence,
        candidates,
    }
}

/// Scores the windows the block could start at, best first, and reports how many
/// plausible starts the file has in total.
///
/// Windows are anchored on the block's first and last line: those two
/// comparisons alone find the block boundaries when only whitespace differs, so
/// the common case never pays for an edit-distance pass per line. The count is
/// what the caller uses to refuse a block whose boundaries are everywhere
/// (FR-TOOL-EDIT-4).
fn candidates(source: &[(usize, String)], block: &[String]) -> (usize, Vec<Candidate>) {
    let first = &block[0];
    let last = &block[block.len() - 1];
    let size = block.len();
    let mut starts: Vec<usize> = source
        .iter()
        .enumerate()
        .filter(|(index, (_, line))| {
            line == first
                && index.saturating_add(size) <= source.len()
                && source[index + size - 1].1 == *last
        })
        .map(|(index, _)| index)
        .collect();
    if starts.is_empty() {
        starts = nearest_lines(source, first);
    }
    starts.retain(|start| start.saturating_add(size) <= source.len());

    let wanted = block.join("\n");
    let scored: Vec<Candidate> = starts
        .iter()
        .take(MAX_ANCHORS)
        .filter_map(|start| {
            let window = source.get(*start..start + size)?;
            Some(Candidate {
                score: similarity(&join_lines(window), &wanted),
                first: window.first()?.0,
                last: window.last()?.0,
            })
        })
        .collect();
    let mut scored = if scored.is_empty() {
        whole_file(source, &wanted).into_iter().collect()
    } else {
        scored
    };
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then(left.first.cmp(&right.first))
    });
    (starts.len(), scored)
}

/// The lines whose text is closest to the block's first line, used when nothing
/// matches it. Byte length is used as a cheap band first: a line that cannot be
/// close in length cannot be close in content.
fn nearest_lines(source: &[(usize, String)], anchor: &str) -> Vec<usize> {
    let band = anchor.len().saturating_mul(2).max(8);
    let mut scored: Vec<(f64, usize)> = source
        .iter()
        .enumerate()
        .filter(|(_, (_, line))| line.len() <= band)
        .map(|(index, (_, line))| (similarity(line, anchor), index))
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
            .then(left.1.cmp(&right.1))
    });
    scored
        .into_iter()
        .take(MAX_ANCHORS)
        .map(|(_, index)| index)
        .collect()
}

/// A file with fewer lines than the block: showing the whole of it is more
/// useful than showing nothing.
fn whole_file(source: &[(usize, String)], wanted: &str) -> Option<Candidate> {
    let first = source.first()?.0;
    let last = source.last()?.0;
    Some(Candidate {
        score: similarity(&join_lines(source), wanted),
        first,
        last,
    })
}

fn join_lines(lines: &[(usize, String)]) -> String {
    lines
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replaces lines `first..=last` with `replacement`, keeping the line
/// terminator the removed block ended with so the following line stays whole
/// (FR-TOOL-FILE-2).
fn splice_lines(content: &mut String, first: usize, last: usize, replacement: &str) {
    let starts = line_starts(content);
    let start = starts.get(first).copied().unwrap_or(content.len());
    let end = starts.get(last + 1).copied().unwrap_or(content.len());
    let terminated = content
        .get(start..end)
        .is_some_and(|removed| removed.ends_with('\n'));
    let mut text = replacement.to_owned();
    if terminated && !text.is_empty() && !text.ends_with('\n') {
        text.push_str(eol_of(content));
    }
    content.replace_range(start..end, &text);
}

/// The terminator the file already uses: CRLF when it has any, LF otherwise.
fn eol_of(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Rewrites a replacement's terminators to match the file, so an edit never
/// mixes endings into a file that uses CRLF.
fn normalize_newlines(text: &str, eol: &str) -> String {
    if eol == "\n" {
        text.to_owned()
    } else {
        text.replace("\r\n", "\n").replace('\n', eol)
    }
}

/// Byte offset of the start of every line, plus one past the end of the text.
fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0_usize];
    for (offset, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(offset + 1);
        }
    }
    starts
}

/// The 0-based line that contains `offset`.
fn line_of(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    }
}

fn line_texts(content: &str) -> Vec<&str> {
    content.lines().collect()
}

/// Every non-blank line of the file, normalised, with its 0-based line number.
fn normalized_source(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .enumerate()
        .map(|(number, line)| (number, normalize_line(line)))
        .filter(|(_, normalized)| !normalized.is_empty())
        .collect()
}

fn normalized_block(text: &str) -> Vec<String> {
    text.lines()
        .map(normalize_line)
        .filter(|line| !line.is_empty())
        .collect()
}

/// Collapses whitespace runs to a single space: two lines that differ only in
/// indentation or in the spacing between tokens become the same string.
fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Levenshtein similarity of two strings, `1.0` when they are identical.
///
/// The distance is at least the difference in length, which is what lets the
/// caller skip hopeless candidates cheaply.
fn similarity(left: &str, right: &str) -> f64 {
    let distance = levenshtein(left, right);
    let longest = left.chars().count().max(right.chars().count()).max(1);
    1.0 - as_f64(distance) / as_f64(longest)
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0_usize; right.len() + 1];
    for (row, left_char) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[column + 1] = (previous[column] + cost)
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// `usize as f64` is a precision-loss cast and the lint policy forbids them, so
/// the conversion is explicit and saturating.
fn as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

/// The lines around `first..=last`, with the line numbers the file has now,
/// clipped so one long line cannot push the rest out of the report.
fn snippet(lines: &[&str], first: usize, last: usize, context: usize) -> Snippet {
    let start = first.saturating_sub(context);
    let end = last
        .saturating_add(context)
        .min(lines.len().saturating_sub(1));
    let shown: &[&str] = lines.get(start..=end).unwrap_or_default();
    let kept = shown.len().min(MAX_SNIPPET_LINES);
    Snippet {
        start_line: start + 1,
        lines: shown.iter().take(kept).map(|line| clip(line)).collect(),
        skipped: shown.len().saturating_sub(kept),
    }
}

fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_WIDTH) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

fn render(outcome: &EditOutcome) -> Result<ToolOutput> {
    audit(outcome);
    Ok(ToolOutput {
        parts: vec![Part::text(describe(outcome))],
        structured: Some(serde_json::to_value(outcome)?),
        is_error: outcome.code != EditCode::Applied,
    })
}

fn describe(outcome: &EditOutcome) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    if outcome.code == EditCode::Applied {
        let _ = writeln!(
            text,
            "edited {}: {} edit(s) applied ({} lines, {} bytes)",
            outcome.path.display(),
            outcome.matches.len(),
            outcome.total_lines,
            outcome.bytes
        );
        for entry in &outcome.matches {
            let _ = writeln!(
                text,
                "  edit {}: {} at lines {}-{}",
                entry.edit,
                match_text(entry),
                entry.start_line,
                entry.end_line
            );
        }
        return text;
    }

    let Some(failure) = &outcome.failure else {
        return text;
    };
    let _ = writeln!(
        text,
        "edit failed: nothing was written to {}",
        outcome.path.display()
    );
    match failure.reason {
        EditFailureReason::Ambiguous => {
            let _ = writeln!(
                text,
                "edit {} matches {} place(s); make the block unique by widening it",
                failure.edit, failure.occurrences
            );
        }
        EditFailureReason::NoMatch => {
            let _ = writeln!(
                text,
                "edit {} has no exact match for its {} line(s){}; retry with what the file actually has",
                failure.edit,
                failure.search_lines,
                closeness(failure)
            );
        }
    }
    for candidate in &failure.candidates {
        let last = candidate.start_line + candidate.lines.len().saturating_sub(1);
        let _ = writeln!(text, "lines {}-{last}:", candidate.start_line);
        for (offset, line) in candidate.lines.iter().enumerate() {
            let _ = writeln!(text, "{:>5} | {line}", candidate.start_line + offset);
        }
        if candidate.skipped > 0 {
            let _ = writeln!(text, "        … {} more line(s)", candidate.skipped);
        }
    }
    text
}

fn match_text(entry: &EditMatch) -> String {
    match entry.kind {
        MatchKind::Exact => "exact".to_owned(),
        MatchKind::Whitespace => "whitespace".to_owned(),
        MatchKind::Fuzzy => format!("fuzzy {:.0}%", entry.confidence * 100.0),
    }
}

fn closeness(failure: &EditFailure) -> String {
    failure.confidence.map_or_else(String::new, |confidence| {
        format!(
            "; the closest region is {:.0}% similar",
            confidence.clamp(0.0, 1.0) * 100.0
        )
    })
}

fn audit(outcome: &EditOutcome) {
    tracing::info!(
        target: "rustychain.audit",
        tool = "edit",
        code = ?outcome.code,
        path = %outcome.path.display(),
        written = outcome.written,
        applied = outcome.matches.len(),
        content_digest = %outcome.content_digest,
        request_digest = %outcome.request_digest,
        "file edited"
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const SOURCE: &str = concat!(
        "use std::fmt;\n",
        "\n",
        "pub struct Config {\n",
        "    pub port: u16,\n",
        "}\n",
        "\n",
        "pub fn main() {\n",
        "    let config = Config { port: 80 };\n",
        "    println!(\"{config:?}\");\n",
        "}\n",
    );

    fn scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ticket = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "rustychain-edit-{name}-{}-{ticket}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("scratch");
        directory
    }

    /// A file seeded with `content`, and the request that edits it.
    fn seeded(name: &str, content: &str, specs: Vec<(&str, &str)>) -> (PathBuf, EditRequest) {
        let path = scratch(name).join("main.rs");
        std::fs::write(&path, content).expect("seed");
        let request = EditRequest {
            path: path.clone(),
            edits: specs
                .into_iter()
                .map(|(search, replace)| EditSpec {
                    search: search.to_owned(),
                    replace: replace.to_owned(),
                })
                .collect(),
        };
        (path, request)
    }

    async fn run(request: &EditRequest) -> ToolOutput {
        EditTool::new().call(request.clone()).await.expect("edit")
    }

    fn structured(output: &ToolOutput) -> serde_json::Value {
        output.structured.clone().expect("structured")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read")
    }

    #[tokio::test]
    async fn an_exact_block_is_replaced() {
        let (path, request) = seeded(
            "exact",
            SOURCE,
            vec![(
                "pub struct Config {\n    pub port: u16,\n}",
                "pub struct Config {\n    pub port: u16,\n    pub host: String,\n}",
            )],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "applied");
        assert_eq!(structured["written"], true);
        assert_eq!(structured["matches"][0]["kind"], "exact");
        assert_eq!(structured["matches"][0]["start_line"], 3);
        assert_eq!(structured["matches"][0]["end_line"], 5);
        assert_eq!(structured["matches"][0]["confidence"], 1.0);
        assert_eq!(
            read(&path),
            SOURCE.replace(
                "    pub port: u16,\n}",
                "    pub port: u16,\n    pub host: String,\n}"
            )
        );
    }

    #[tokio::test]
    async fn a_block_that_only_differs_in_indentation_still_applies() {
        let (path, request) = seeded(
            "whitespace",
            SOURCE,
            vec![(
                "pub struct Config {\npub port: u16,\n}",
                "pub struct Config {\n    pub port: u16,\n    pub host: String,\n}",
            )],
        );
        let output = run(&request).await;
        assert!(
            !output.is_error,
            "indentation alone must not defeat an edit"
        );
        let structured = structured(&output);
        assert_eq!(structured["matches"][0]["kind"], "whitespace");
        assert_eq!(structured["matches"][0]["confidence"], 1.0);
        assert_eq!(structured["matches"][0]["start_line"], 3);
        assert_eq!(
            read(&path),
            SOURCE.replace(
                "    pub port: u16,\n}",
                "    pub port: u16,\n    pub host: String,\n}"
            )
        );
    }

    #[tokio::test]
    async fn a_block_close_enough_applies_as_fuzzy() {
        let (path, request) = seeded(
            "fuzzy",
            SOURCE,
            vec![(
                "pub struct Config {\n    pub port: u16;\n}",
                "pub struct Config {\n    pub port: u32,\n}",
            )],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["matches"][0]["kind"], "fuzzy");
        let confidence = structured["matches"][0]["confidence"]
            .as_f64()
            .expect("confidence");
        assert!(
            (0.95..1.0).contains(&confidence),
            "a one-character difference is above the threshold, got {confidence}"
        );
        assert_eq!(
            read(&path),
            SOURCE.replace("pub port: u16,", "pub port: u32,")
        );
    }

    #[tokio::test]
    async fn a_block_below_the_threshold_reports_the_current_lines() {
        let (path, request) = seeded(
            "nomatch",
            SOURCE,
            vec![(
                "pub struct Settings {\n    pub port: u16,\n    pub retries: u8,\n}",
                "pub struct Settings;",
            )],
        );
        let before = read(&path);
        let output = run(&request).await;
        assert!(
            output.is_error,
            "a miss is a failed result (FR-TOOL-EDIT-5)"
        );
        let structured = structured(&output);
        assert_eq!(structured["code"], "no_match");
        assert_eq!(structured["written"], false);
        assert_eq!(
            read(&path),
            before,
            "a failed request never touches the file"
        );
        assert_eq!(structured["failure"]["edit"], 0);
        assert_eq!(structured["failure"]["search_lines"], 4);
        let confidence = structured["failure"]["confidence"]
            .as_f64()
            .expect("confidence");
        assert!(confidence < 0.95, "got {confidence}");

        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(
            text.contains("pub struct Config {") && text.contains("    pub port: u16,"),
            "the report shows what the file has: {text}"
        );
        assert!(text.contains("    3 |"), "with line numbers: {text}");
        assert!(
            text.contains("no exact match for its 4 line(s)"),
            "the report says how big the search was: {text}"
        );
    }

    #[tokio::test]
    async fn an_ambiguous_search_lists_every_occurrence() {
        let duplicated = format!("{SOURCE}\n{SOURCE}");
        let (path, request) = seeded(
            "ambiguous",
            &duplicated,
            vec![("pub struct Config {", "pub struct Settings {")],
        );
        let before = read(&path);
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["occurrences"], 2);
        assert_eq!(
            structured["failure"]["candidates"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(read(&path), before);

        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(text.contains("matches 2 place(s)"), "{text}");
        assert!(
            text.contains("    3 |") && text.contains("   13 |"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn two_equally_close_blocks_are_ambiguous() {
        let content = "fn one() {\n    let a = 1;\n}\n\nfn one() {\n  let a = 1;\n}\n";
        let (path, request) = seeded(
            "tie",
            content,
            vec![(
                "fn one() {\n   let a = 1;\n}",
                "fn two() {\n    let a = 2;\n}",
            )],
        );
        let output = run(&request).await;
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["occurrences"], 2);
        assert_eq!(read(&path), content, "neither block was rewritten");
    }

    #[tokio::test]
    async fn edits_apply_in_order_and_see_each_other() {
        let (path, request) = seeded(
            "order",
            "alpha\nomega\n",
            vec![
                ("alpha\n", "alpha\nmiddle\n"),
                ("middle\nomega", "middle\nTWO\nomega"),
            ],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&path), "alpha\nmiddle\nTWO\nomega\n");
        let structured = structured(&output);
        assert_eq!(structured["matches"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn one_edit_that_fails_leaves_the_file_untouched() {
        let (path, request) = seeded(
            "partial",
            SOURCE,
            vec![
                ("pub port: u16,", "pub port: u32,"),
                ("this text is nowhere in the file", "replacement"),
            ],
        );
        let before = read(&path);
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["failure"]["edit"], 1);
        assert_eq!(
            structured["matches"].as_array().map(Vec::len),
            Some(1),
            "the first edit matched in memory only"
        );
        assert_eq!(read(&path), before, "and was never written");
    }

    #[tokio::test]
    async fn deleting_a_block_removes_its_lines() {
        let (path, request) = seeded(
            "delete",
            SOURCE,
            vec![("    println!(\"{config:?}\");\n", "")],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        assert!(!read(&path).contains("println"));
        assert!(
            read(&path).ends_with("};\n}\n"),
            "the following lines stay whole: {:?}",
            read(&path)
        );
    }

    #[tokio::test]
    async fn replacing_a_whole_file_with_nothing_leaves_it_empty() {
        let (path, request) = seeded("empty", "only line\n", vec![("only line\n", "")]);
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&path), "");
        assert_eq!(structured(&output)["total_lines"], 0);
    }

    #[tokio::test]
    async fn a_file_without_a_trailing_newline_keeps_it() {
        let (path, request) = seeded("noeol", "abc", vec![("abc", "xyz")]);
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&path), "xyz", "no terminator is invented");
    }

    #[tokio::test]
    async fn a_crlf_file_stays_crlf() {
        let (path, request) = seeded(
            "crlf",
            "one\r\ntwo\r\nthree\r\n",
            vec![("two\nthree", "two\nTWO")],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        let written = read(&path);
        assert_eq!(written, "one\r\ntwo\r\nTWO\r\n");
        assert!(
            !written.replace("\r\n", "").contains('\n'),
            "no bare LF was left behind"
        );
        assert_eq!(structured(&output)["matches"][0]["kind"], "whitespace");
    }

    #[tokio::test]
    async fn the_fallback_can_be_disabled() {
        let (path, request) = seeded(
            "disabled",
            SOURCE,
            vec![("pub struct Config {\npub port: u16,\n}", "replaced")],
        );
        let limits = EditLimits {
            fuzzy_threshold: None,
            ..EditLimits::default()
        };
        let output = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect("edit");
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "no_match");
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn the_threshold_can_be_raised_to_exact_whitespace_only() {
        let (path, request) = seeded(
            "strict",
            SOURCE,
            vec![("pub struct Config {\n    pub port: u16;\n}", "replaced")],
        );
        let limits = EditLimits {
            fuzzy_threshold: Some(1.0),
            ..EditLimits::default()
        };
        let output = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect("edit");
        assert!(
            output.is_error,
            "a one-character difference is no longer enough"
        );
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn a_multibyte_block_is_compared_by_characters() {
        let content = "let saludo = \"añáé\";\nlet otro = 1;\n";
        let (path, request) = seeded(
            "multibyte",
            content,
            vec![(
                "let saludo = \"añáé\";\nlet otro = 2;",
                "let saludo = \"adiós\";\nlet otro = 2;",
            )],
        );
        let output = run(&request).await;
        assert!(
            !output.is_error,
            "multibyte text must not break the fallback"
        );
        assert_eq!(read(&path), "let saludo = \"adiós\";\nlet otro = 2;\n");
        assert_eq!(structured(&output)["matches"][0]["kind"], "fuzzy");
    }

    #[tokio::test]
    async fn a_long_line_is_clipped_in_the_report() {
        let long = "x".repeat(400);
        let content = format!("fn a() {{}}\nlet line = \"{long}\";\n");
        let (_, request) = seeded("clip", &content, vec![("nowhere near this", "replacement")]);
        let output = run(&request).await;
        assert!(output.is_error);
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(text.contains('…'), "the report clips: {text}");
        assert!(
            text.lines().all(|line| line.chars().count() < 260),
            "no reported line runs away"
        );
    }

    #[tokio::test]
    async fn a_file_over_the_size_limit_is_refused() {
        let (_, request) = seeded("big", SOURCE, vec![("a", "b")]);
        let limits = EditLimits {
            max_file_bytes: 4,
            ..EditLimits::default()
        };
        let error = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect_err("over the limit");
        assert!(matches!(error, Error::LimitExceeded { limit: 4, .. }));
    }

    #[tokio::test]
    async fn a_missing_file_names_the_path() {
        let request = EditRequest {
            path: scratch("missing").join("absent.rs"),
            edits: vec![EditSpec {
                search: "a".to_owned(),
                replace: "b".to_owned(),
            }],
        };
        let error = EditTool::new().call(request).await.expect_err("missing");
        assert!(error.to_string().contains("absent.rs"));
    }

    #[tokio::test]
    async fn an_empty_search_is_invalid_input() {
        let (_, request) = seeded("blank", SOURCE, vec![("   \n", "replacement")]);
        let error = EditTool::new().call(request).await.expect_err("blank");
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("empty search")));
    }

    #[tokio::test]
    async fn a_request_without_edits_is_invalid_input() {
        let request = EditRequest {
            path: scratch("noedits").join("main.rs"),
            edits: Vec::new(),
        };
        let error = EditTool::new().call(request).await.expect_err("no edits");
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("at least one")));
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&EditTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("edit").expect("name");
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(spec.description, <EditTool as ToolMetadata>::DESCRIPTION);

        let path = scratch("envelope").join("main.rs");
        std::fs::write(&path, "let a = 1;\n").expect("seed");
        let output = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": path,
                    "edits": [{ "search": "let a = 1;", "replace": "let a = 2;" }]
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        assert!(!output.parts.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "let a = 2;\n"
        );
    }

    #[tokio::test]
    async fn unknown_arguments_are_rejected_before_the_body_runs() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&EditTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("edit").expect("name");
        let error = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": "ignored.rs",
                    "edits": [{ "search": "a", "replace": "b" }],
                    "whole_file": true
                }),
            )
            .await
            .expect_err("unknown field (FR-TOOL-2)");
        assert!(matches!(error, Error::InvalidToolArguments { .. }));
    }

    #[test]
    fn the_similarity_is_a_ratio_of_characters() {
        assert!((similarity("abc", "abc") - 1.0).abs() < f64::EPSILON);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("añáé", "añáé"), 0);
        assert!((similarity("abc", "abd") - 2.0 / 3.0).abs() < 1e-9);
        assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
    }
}
