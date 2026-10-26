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

use norte_core::{
    Digest, Error, FnExecutor, Part, Result, ToolDeclaration, ToolMetadata, ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fs::{io_error, read_bytes, read_text, write_atomic};
use crate::text::{clip, escape};

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
/// Character comparisons one call may spend comparing a block with the file, so
/// a block that belongs nowhere cannot hold the executor — and the runtime it
/// runs on — for as long as it likes (FR-TOOL-EDIT-3).
const SCORING_BUDGET: usize = 32 * 1024 * 1024;

/// One search-and-replace pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(description = "One search-and-replace pair. search and replace must differ.")]
pub struct EditSpec {
    /// The text to find, exactly once in the file.
    #[schemars(
        description = "The text to find. It has to match exactly once; keep it to the few lines that make it unique instead of copying a large block. When nothing matches exactly, a block that differs only in whitespace still applies."
    )]
    pub search: String,
    /// What replaces it; it has to differ from `search`.
    #[schemars(
        description = "The new text that replaces search. It must differ from search: an edit that would leave the file as it is fails. An empty string deletes the block."
    )]
    pub replace: String,
    /// Which of several identical places to change, counting from one.
    ///
    /// Only needed when `search` matches more than once, which is the case the
    /// tool otherwise refuses: two identical lines a file apart are a block that
    /// cannot be told from its twin by any amount of surrounding context. Naming
    /// the occurrence is what saves copying twenty lines of neighbourhood to
    /// disambiguate something the file already numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Which occurrence to change when search matches more than once, counting from 1 in the order they appear in the file. Leave it out when search is unique; the failure lists the occurrences when it is not."
    )]
    pub occurrence: Option<usize>,
}

impl EditSpec {
    /// A pair that finds `search` and puts `replace` in its place.
    #[must_use]
    pub fn new(search: impl Into<String>, replace: impl Into<String>) -> Self {
        Self {
            search: search.into(),
            replace: replace.into(),
            occurrence: None,
        }
    }

    /// The same pair, aimed at the `nth` of several identical places.
    #[must_use]
    pub fn nth(mut self, nth: usize) -> Self {
        self.occurrence = Some(nth);
        self
    }
}

/// The edits to apply to one file, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(description = "The edits to apply to one file, in order.")]
#[non_exhaustive]
pub struct EditRequest {
    /// The file to edit.
    #[schemars(description = "The file to edit.", example = &"src/main.rs")]
    pub path: PathBuf,
    /// Applied in order, each one over the result of the previous.
    #[schemars(
        description = "Applied in order, each one over the result of the previous. Nothing is written unless every edit applies and changes the file.",
        example = serde_json::json!([{"search": "let x = 1;", "replace": "let x = 2;"}])
    )]
    pub edits: Vec<EditSpec>,
}

impl EditRequest {
    /// A request that applies one pair to `path`.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        search: impl Into<String>,
        replace: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            edits: vec![EditSpec::new(search, replace)],
        }
    }

    /// Adds a pair, applied after the ones already there.
    #[must_use]
    pub fn with_edit(mut self, search: impl Into<String>, replace: impl Into<String>) -> Self {
        self.edits.push(EditSpec::new(search, replace));
        self
    }

    /// Adds a pair aimed at one of several identical places.
    #[must_use]
    pub fn with_nth_edit(
        mut self,
        search: impl Into<String>,
        replace: impl Into<String>,
        nth: usize,
    ) -> Self {
        self.edits.push(EditSpec::new(search, replace).nth(nth));
        self
    }
}

/// The outcome code of an edit request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditCode {
    /// Every edit applied and the file was written.
    Applied,
    /// An edit, or the request as a whole, would have left the file exactly as
    /// it was. That is never what an edit is for, so it is a failure the model
    /// is told about, not a success with nothing in it. Nothing was written.
    Unchanged,
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
    /// How the block was matched.
    pub kind: MatchKind,
    /// 1-based first line of the replaced block, in the file as the edits
    /// before this one would have left it.
    pub start_line: usize,
    /// 1-based last line of the replaced block, in the same state.
    pub end_line: usize,
    /// Similarity of the searched block to the block that was there: `1.0` for
    /// an exact match.
    pub confidence: f64,
}

/// The lines around a candidate, for a model that has to retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// 1-based number of the first line. The numbers are those of the text the
    /// edit was applied to: the file itself when the call failed on its first
    /// edit, and the file as the earlier edits of the same call would have left
    /// it otherwise.
    pub start_line: usize,
    /// The lines, without terminators, with the characters a terminal would act
    /// on escaped and clipped to a readable width — a trailing `…` marks a line
    /// the width cut.
    pub lines: Vec<String>,
    /// Lines of the region that were left out for being too long.
    pub skipped: usize,
}

/// Why an edit failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditFailureReason {
    /// Nothing matched exactly and no block was close enough.
    NoMatch,
    /// The block, or several equally close blocks, matched in more than one
    /// place.
    Ambiguous,
    /// The block matched once, but its replacement is what the file already
    /// has there, so the edit would change nothing.
    Unchanged,
    /// The request named an occurrence the file does not have: it asked for the
    /// fourth of three.
    NoSuchOccurrence,
}

/// Why an edit did not apply, with the evidence to retry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditFailure {
    /// Index of the edit that failed.
    pub edit: usize,
    /// The reason the edit failed.
    pub reason: EditFailureReason,
    /// How many lines the searched block has, blank lines excluded.
    pub search_lines: usize,
    /// How many places matched the block, or tied with the closest one: zero
    /// when nothing matched, and a count past the executor's anchor cap means
    /// "that many or more".
    pub occurrences: usize,
    /// The occurrence the request asked for, when it asked for one and the file
    /// does not have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_occurrence: Option<usize>,
    /// Similarity of the best candidate to the searched block, when one was
    /// found at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// The text around the places that mattered, taken from the copy the edit
    /// was applied to (see [`Snippet::start_line`]).
    pub candidates: Vec<Snippet>,
}

/// What one edit request did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditOutcome {
    /// The outcome code.
    pub code: EditCode,
    /// The file the request named.
    pub path: PathBuf,
    /// The edits that matched, in order. On a failure, the ones before it.
    pub matches: Vec<EditMatch>,
    /// The evidence for a retry, on a failure. Absent on an
    /// [`EditCode::Unchanged`] whose edits each changed something but together
    /// cancelled out, where no single edit is to blame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<EditFailure>,
    /// Whether the file on disk was rewritten: false on any failure, including
    /// [`EditCode::Unchanged`], so a no-op is never told to the model as a
    /// change.
    pub written: bool,
    /// Lines and bytes of the file after the call, written or not.
    pub total_lines: usize,
    /// Bytes in the file after the call.
    pub bytes: usize,
    /// Digest of the content the call worked from, whatever the outcome, so a
    /// host correlating its own records can tell which version an edit replaced.
    pub before_digest: Digest,
    /// Digest of the file the call left, written or not.
    pub content_digest: Digest,
    /// Digest of the request as the executor judged it.
    pub request_digest: Digest,
}

/// Bounds for the edit executor (FR-TOOL-EDIT-2, FR-TOOL-EDIT-3,
/// FR-TOOL-FILE-1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditLimits {
    /// Refuse to edit a file larger than this.
    pub max_file_bytes: usize,
    /// How many lines of context a failure shows around a candidate.
    pub context_lines: usize,
    /// Similarity a block that is not an exact match needs to apply, between
    /// `0.0` and `1.0`. `None` disables the fallback and leaves only exact,
    /// unique matches. `Some(1.0)`, the default, is what the tool tells the
    /// model: a block that differs only in indentation or whitespace applies,
    /// and one that differs in content does not — a lower value also accepts a
    /// block that far off, which is a host's decision to make, not a default.
    ///
    /// A value outside `0.0 ..= 1.0` is read as `1.0`, and a value that is not
    /// a number as `None`: a threshold is a bound on how wrong a block may be,
    /// and it cannot be widened by asking for more than the maximum.
    pub fuzzy_threshold: Option<f64>,
    /// Refuse a `search` or a `replace` longer than this, so one edit cannot
    /// carry a block nobody could match and no comparison has to be paid for
    /// that could not have succeeded.
    pub max_block_bytes: usize,
    /// Refuse a request with more edits than this.
    pub max_edits: usize,
}

impl Default for EditLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            context_lines: 3,
            fuzzy_threshold: Some(1.0),
            max_block_bytes: 16 * 1024,
            max_edits: 64,
        }
    }
}

/// Changes a text file by search and replace (FR-TOOL-EDIT-1).
///
/// The path comes from the request: what may be edited is the host's policy
/// (FR-POL-1), not the executor's business.
#[norte_core::tool(
    name = "edit",
    description = "Change a text file with search and replace. Each edit's search has to match exactly once, and its replace has to differ from it: an edit that would leave the file as it is fails. Keep search to the few lines that make it unique rather than copying a large block. A block that differs only in indentation or whitespace still applies. Nothing is written unless every edit applies. A failure reports the lines around the closest region, so the next attempt can be exact."
)]
pub struct EditTool {
    limits: EditLimits,
}

impl EditTool {
    /// An edit tool with the default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: EditLimits::default(),
        }
    }

    /// Sets the bounds on what may be edited.
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
        validate(request, &self.limits)?;
        let before = read_text(&request.path, self.limits.max_file_bytes)?;
        let before_digest = Digest::of(before.as_bytes());
        let request_digest = Digest::of_json(&serde_json::to_value(request)?);
        let (outcome, hint) = match self.apply(&before, request) {
            Ok((content, matches)) => {
                let changed = content != before;
                if changed {
                    ensure_unchanged(&request.path, &before_digest, self.limits.max_file_bytes)?;
                    write_atomic(&request.path, content.as_bytes())?;
                }
                (
                    EditOutcome {
                        code: if changed {
                            EditCode::Applied
                        } else {
                            EditCode::Unchanged
                        },
                        path: request.path.clone(),
                        matches,
                        failure: None,
                        written: changed,
                        total_lines: content.lines().count(),
                        bytes: content.len(),
                        before_digest,
                        content_digest: Digest::of(content.as_bytes()),
                        request_digest,
                    },
                    None,
                )
            }
            Err(Rejection {
                applied,
                failure,
                hint,
            }) => (
                EditOutcome {
                    code: code_of(failure.reason),
                    path: request.path.clone(),
                    matches: applied,
                    failure: Some(failure),
                    written: false,
                    total_lines: before.lines().count(),
                    bytes: before.len(),
                    content_digest: before_digest,
                    before_digest,
                    request_digest,
                },
                hint,
            ),
        };
        render(&outcome, hint.as_deref())
    }

    /// Applies every edit to an in-memory copy, or reports the first one that
    /// does not apply (FR-TOOL-EDIT-1).
    ///
    /// The rejection is returned by value because it is the answer, not an
    /// error: a failure carries the snippets the model reads to retry. Boxing it
    /// would put an allocation on the path of every failed edit to satisfy a
    /// size lint, and the type is bounded — a handful of snippets and a hint.
    #[allow(clippy::result_large_err)]
    fn apply(
        &self,
        before: &str,
        request: &EditRequest,
    ) -> std::result::Result<(String, Vec<EditMatch>), Rejection> {
        let budget = Budget::new();
        let mut content = before.to_owned();
        let mut matches = Vec::with_capacity(request.edits.len());
        for (index, spec) in request.edits.iter().enumerate() {
            match apply_one(&mut content, index, spec, &self.limits, &budget) {
                Ok(entry) => matches.push(entry),
                Err(failure) => {
                    let hint = if failure.reason == EditFailureReason::NoMatch {
                        escape_hint(&spec.search, &content)
                    } else {
                        None
                    };
                    return Err(Rejection {
                        applied: matches,
                        failure,
                        hint,
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
pub fn edit_tool() -> Arc<dyn norte_core::AnyFunction> {
    Arc::new(EditTool::new().into_declaration())
}

/// The edits that did apply before one failed, with the evidence for it.
struct Rejection {
    applied: Vec<EditMatch>,
    failure: EditFailure,
    /// A retry hint that does not belong in the serialized failure, such as a
    /// literal `\n` that probably meant a line break.
    hint: Option<String>,
}

/// The engine's own result for one edit.
type MatchResult = std::result::Result<EditMatch, EditFailure>;

/// One window of source lines that could be the searched block.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    score: f64,
    /// 0-based line number of the first line, in the file.
    first: usize,
    /// 0-based line number of the last line, in the file.
    last: usize,
    /// Where the window starts in the normalised source, which is what lines
    /// the block's own lines were compared with.
    start: usize,
}

/// Argument-level checks, run before the file is read (FR-TOOL-EDIT-2).
fn validate(request: &EditRequest, limits: &EditLimits) -> Result<()> {
    if request.path.as_os_str().is_empty() {
        return Err(Error::InvalidInput("edit: a path is required".to_owned()));
    }
    if request.edits.is_empty() {
        return Err(Error::InvalidInput(
            "edit: at least one edit is required".to_owned(),
        ));
    }
    if request.edits.len() > limits.max_edits {
        return Err(Error::InvalidInput(format!(
            "edit: {} edits in one call is over the limit of {}",
            request.edits.len(),
            limits.max_edits
        )));
    }
    for (index, spec) in request.edits.iter().enumerate() {
        if spec.search.is_empty() {
            return Err(Error::InvalidInput(format!(
                "edit: edit {index} has an empty search block"
            )));
        }
        if spec.search == spec.replace {
            return Err(Error::InvalidInput(format!(
                "edit: edit {index} has the same text in search and replace, so it would change \
                 nothing; put the new text in replace"
            )));
        }
        if spec.search.len() > limits.max_block_bytes || spec.replace.len() > limits.max_block_bytes
        {
            return Err(Error::InvalidInput(format!(
                "edit: edit {index} has a block over the limit of {} bytes",
                limits.max_block_bytes
            )));
        }
        // Counting from one, as the file numbers them. A zero is a request that
        // meant to say "the first" and said something else.
        if spec.occurrence == Some(0) {
            return Err(Error::InvalidInput(format!(
                "edit: edit {index} asks for occurrence 0; occurrences are counted from 1"
            )));
        }
    }
    Ok(())
}

/// What one call may spend comparing a block with the file, so the work a
/// request can ask for is bounded before it starts (FR-TOOL-EDIT-3).
struct Budget {
    left: std::cell::Cell<usize>,
}

impl Budget {
    fn new() -> Self {
        Self {
            left: std::cell::Cell::new(SCORING_BUDGET),
        }
    }

    /// Charges `cost` character comparisons, and answers whether they were
    /// affordable: a comparison nobody can pay for is not made at all.
    fn charge(&self, cost: usize) -> bool {
        let left = self.left.get();
        if cost > left {
            return false;
        }
        self.left.set(left - cost);
        true
    }
}

/// Refuses to write when the file changed between the read that the edit was
/// prepared from and the write.
///
/// The window is narrowed, not closed: two writers can pass this check at the
/// same time, and only a lock or an atomic compare-and-swap would close it.
/// What it buys is detection — a version that is no longer there is not
/// silently overwritten — and what the model is told is to read the file again,
/// never that the edit landed.
fn ensure_unchanged(path: &std::path::Path, before: &Digest, max_bytes: usize) -> Result<()> {
    let now = Digest::of(&read_bytes(path, max_bytes)?);
    if now == *before {
        return Ok(());
    }
    Err(io_error(
        path,
        &std::io::Error::other(
            "the file changed while the edit was being prepared, so nothing was written; \
             read it again and retry",
        ),
    ))
}

fn code_of(reason: EditFailureReason) -> EditCode {
    match reason {
        EditFailureReason::NoMatch => EditCode::NoMatch,
        EditFailureReason::Unchanged => EditCode::Unchanged,
        // A number the file does not have is the ambiguous family: the block
        // matched, and the request did not say which match it meant. Why it did
        // not is in the reason.
        EditFailureReason::Ambiguous | EditFailureReason::NoSuchOccurrence => EditCode::Ambiguous,
    }
}

/// Applies one edit in place: exactly one match or nothing at all, and a match
/// whose replacement is what the file already has there is refused, since an
/// edit that changes nothing is a mistake the model has to hear about.
///
/// The rejection is returned by value because it is the answer: a failure
/// carries the snippets the model reads to retry, and boxing it to satisfy a
/// size lint would put an allocation on the path of every failed edit.
fn apply_one(
    content: &mut String,
    index: usize,
    spec: &EditSpec,
    limits: &EditLimits,
    budget: &Budget,
) -> MatchResult {
    let offsets = occurrences(content, &spec.search);
    let starts = line_starts(content);
    // A block that matches several times may name which one it means. That is
    // the whole answer to "two identical lines and no way to tell them apart":
    // the file numbers them, so the request can too.
    if let Some(nth) = spec.occurrence {
        let Some(offset) = offsets.get(nth.wrapping_sub(1)).copied() else {
            return Err(occurrence_out_of_range(
                content, index, spec, &offsets, &starts, limits,
            ));
        };
        let end = offset + spec.search.len();
        let first = line_of(&starts, offset);
        let last = line_of(&starts, end - 1);
        let replacement = normalize_newlines(&spec.replace, eol_at(content, offset));
        if content.get(offset..end) == Some(replacement.as_str()) {
            return Err(unchanged(content, index, spec, (first, last), None, limits));
        }
        content.replace_range(offset..end, &replacement);
        return Ok(EditMatch {
            edit: index,
            kind: MatchKind::Exact,
            start_line: first + 1,
            end_line: last + 1,
            confidence: 1.0,
        });
    }
    match offsets.as_slice() {
        [] => fuzzy_apply(content, index, spec, limits, budget),
        [offset] => {
            let end = *offset + spec.search.len();
            let first = line_of(&starts, *offset);
            let last = line_of(&starts, end - 1);
            let replacement = normalize_newlines(&spec.replace, eol_at(content, *offset));
            if content.get(*offset..end) == Some(replacement.as_str()) {
                return Err(unchanged(content, index, spec, (first, last), None, limits));
            }
            content.replace_range(*offset..end, &replacement);
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

/// The failure for an `occurrence` the file does not have.
///
/// The count is the useful part: a request that asked for the fourth of three
/// needs to be told there are three, and where they are.
fn occurrence_out_of_range(
    content: &str,
    index: usize,
    spec: &EditSpec,
    offsets: &[usize],
    starts: &[usize],
    limits: &EditLimits,
) -> EditFailure {
    let lines = line_texts(content);
    let spans: Vec<(usize, usize)> = offsets
        .iter()
        .map(|offset| {
            (
                line_of(starts, *offset),
                line_of(starts, offset + spec.search.len() - 1),
            )
        })
        .collect();
    EditFailure {
        edit: index,
        reason: EditFailureReason::NoSuchOccurrence,
        search_lines: normalized_block(&spec.search).len(),
        occurrences: offsets.len(),
        // What was asked for, not what there is: the message has to say both, or
        // it cannot say why the number is wrong.
        asked_occurrence: spec.occurrence,
        confidence: None,
        candidates: shown_snippets(&spans, &lines, limits.context_lines),
    }
}

/// How far another window has to be from the best one before the executor is
/// willing to call the best one the block's home, when the match is by content
/// rather than by whitespace.
const FUZZY_MARGIN: f64 = 0.02;

/// The fallback: compare whole lines with whitespace runs collapsed, so a block
/// off by indentation alone still lands (FR-TOOL-EDIT-3).
fn fuzzy_apply(
    content: &mut String,
    index: usize,
    spec: &EditSpec,
    limits: &EditLimits,
    budget: &Budget,
) -> MatchResult {
    let block = normalized_block(&spec.search);
    let floor = threshold(limits);
    if block.is_empty() {
        return Err(no_match(index, &spec.search, None, Vec::new()));
    }
    let source = normalized_source(content);
    let (found, windows) = candidates(&source, &block, floor, budget);
    let Some(best) = windows.first().copied() else {
        return Err(no_match(index, &spec.search, None, Vec::new()));
    };
    let near: Vec<Candidate> = windows
        .iter()
        .copied()
        .filter(|window| is_near(best.score, window.score))
        .collect();
    let reached = floor.is_some_and(|threshold| best.score >= threshold);
    if !reached || near.len() > 1 || found > MAX_ANCHORS {
        let reason = if reached {
            EditFailureReason::Ambiguous
        } else {
            EditFailureReason::NoMatch
        };
        let spans: Vec<(usize, usize)> = (if reached && found <= MAX_ANCHORS {
            &near
        } else {
            &windows
        })
        .iter()
        .map(|window| (window.first, window.last))
        .collect();
        let lines = line_texts(content);
        let candidates = shown_snippets(&spans, &lines, limits.context_lines);
        return Err(EditFailure {
            edit: index,
            reason,
            search_lines: block.len(),
            occurrences: if reached { found.max(near.len()) } else { 0 },
            asked_occurrence: None,
            confidence: Some(best.score),
            candidates,
        });
    }

    let replacement = reindent(content, &source, &best, &spec.search, &spec.replace);
    if !splice_lines(content, best.first, best.last, &replacement) {
        return Err(unchanged(
            content,
            index,
            spec,
            (best.first, best.last),
            Some(best.score),
            limits,
        ));
    }
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

/// The similarity a block needs to apply, read from the limits: a value that is
/// not a number leaves only exact matches, and one outside `0.0 ..= 1.0` is read
/// as `1.0`, the strict end. An unusable threshold can therefore make an edit
/// stricter or exact, never looser — a sloppy match is the failure that is
/// silent, and a refusal is the one the model can act on.
fn threshold(limits: &EditLimits) -> Option<f64> {
    limits
        .fuzzy_threshold
        .filter(|threshold| threshold.is_finite())
        .map(|threshold| {
            if (0.0..=1.0).contains(&threshold) {
                threshold
            } else {
                1.0
            }
        })
}

/// Whether a window is as good as the best one in the executor's eyes: exactly
/// as good when the block's lines match up to whitespace, and within
/// [`FUZZY_MARGIN`] when the match is by content, where a hair's difference is
/// not grounds to prefer one place over another.
fn is_near(best: f64, other: f64) -> bool {
    if best >= 1.0 {
        other >= 1.0
    } else {
        best - other <= FUZZY_MARGIN
    }
}

/// Every place `search` matches, up to a cap: a match has to be unique to
/// apply, so counting further than the cap only wastes work.
///
/// The scan steps one character past each hit rather than one match past it, so
/// two hits that overlap are two places: `aa` in `aaa` is not a unique match,
/// and treating it as one would apply an edit the file cannot say is the one
/// meant (FR-TOOL-EDIT-4).
fn occurrences(content: &str, search: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = content.get(from..).and_then(|rest| rest.find(search)) {
        let offset = from + relative;
        offsets.push(offset);
        if offsets.len() > MAX_OCCURRENCES {
            break;
        }
        let step = content
            .get(offset..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8);
        from = offset + step;
    }
    offsets
}

/// The regions a failure shows: the closest ones first, at most
/// [`REPORTED_SNIPPETS`] of them, and a region that would render exactly like
/// one already shown is dropped — a short file pulls several candidate windows
/// onto the same lines, and the model needs that region once, not once per
/// window (FR-TOOL-EDIT-4).
fn shown_snippets(spans: &[(usize, usize)], lines: &[&str], context: usize) -> Vec<Snippet> {
    let mut shown: Vec<Snippet> = Vec::new();
    for (first, last) in spans {
        let snippet = snippet(lines, *first, *last, context);
        if shown.contains(&snippet) {
            continue;
        }
        shown.push(snippet);
        if shown.len() == REPORTED_SNIPPETS {
            break;
        }
    }
    shown
}

/// The same, without dropping the ones that render alike.
///
/// Two occurrences that look identical are the case this exists for: the whole
/// question is which of them to change, and a report that folds them into one
/// has answered it wrong by saying there is one. Every one gets its own entry,
/// up to the cap, because they are choices rather than evidence.
fn every_snippet(spans: &[(usize, usize)], lines: &[&str], context: usize) -> Vec<Snippet> {
    spans
        .iter()
        .take(REPORTED_SNIPPETS)
        .map(|(first, last)| snippet(lines, *first, *last, context))
        .collect()
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
    let spans: Vec<(usize, usize)> = offsets
        .iter()
        .map(|offset| {
            (
                line_of(starts, *offset),
                line_of(starts, offset + spec.search.len() - 1),
            )
        })
        .collect();
    // One entry per occurrence, and never folded together: two identical lines
    // are two choices, and showing them as one says the opposite.
    let candidates = every_snippet(&spans, &lines, limits.context_lines);
    EditFailure {
        edit: index,
        reason: EditFailureReason::Ambiguous,
        search_lines: normalized_block(&spec.search).len(),
        occurrences: offsets.len(),
        asked_occurrence: None,
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
        occurrences: 0,
        asked_occurrence: None,
        confidence,
        candidates,
    }
}

/// The failure for an edit that found its place and would leave it as it was,
/// showing the lines it matched so the model can see what is already there.
fn unchanged(
    content: &str,
    index: usize,
    spec: &EditSpec,
    span: (usize, usize),
    confidence: Option<f64>,
    limits: &EditLimits,
) -> EditFailure {
    let lines = line_texts(content);
    EditFailure {
        edit: index,
        reason: EditFailureReason::Unchanged,
        search_lines: normalized_block(&spec.search).len(),
        occurrences: 1,
        asked_occurrence: None,
        confidence,
        candidates: shown_snippets(&[span], &lines, limits.context_lines),
    }
}

/// Scores the windows the block could start at, best first, and reports how many
/// plausible starts the file has in total.
///
/// Windows are anchored on the block's first and last line: those two
/// comparisons alone find the block boundaries when only whitespace differs, so
/// the common case never pays for an edit-distance pass per line. The count is
/// what the caller uses to refuse a block whose boundaries are everywhere
/// (FR-TOOL-EDIT-4). When nothing is anchored, the lines closest to the block's
/// first line are tried, each with a length band that a line unable to reach
/// `floor` cannot pass, and every comparison is charged to the call's budget, so
/// a block that belongs nowhere is refused rather than compared with the file
/// until the runtime gives up.
fn candidates(
    source: &[(usize, String)],
    block: &[String],
    floor: Option<f64>,
    budget: &Budget,
) -> (usize, Vec<Candidate>) {
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
        starts = nearest_lines(source, first, floor, budget);
    }
    starts.retain(|start| start.saturating_add(size) <= source.len());

    let wanted = block.join("\n");
    let scored: Vec<Candidate> = starts
        .iter()
        .take(MAX_ANCHORS)
        .filter_map(|start| {
            let window = source.get(*start..start + size)?;
            Some(Candidate {
                score: scored_similarity(budget, &join_lines(window), &wanted)?,
                first: window.first()?.0,
                last: window.last()?.0,
                start: *start,
            })
        })
        .collect();
    let mut scored = if scored.is_empty() {
        whole_file(source, &wanted, budget).into_iter().collect()
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
/// matches it.
///
/// A line can only reach `floor` against the anchor if its length is close
/// enough — the distance between two texts is at least the difference in their
/// lengths, and the similarity is measured against the longer of the two — so
/// the length decides which lines are worth a comparison at all, and the
/// comparison itself is charged to the call's budget.
fn nearest_lines(
    source: &[(usize, String)],
    anchor: &str,
    floor: Option<f64>,
    budget: &Budget,
) -> Vec<usize> {
    let anchor_chars = anchor.chars().count();
    let mut scored: Vec<(f64, usize)> = source
        .iter()
        .enumerate()
        .filter(|(_, (_, line))| within_reach(line, anchor_chars, floor))
        .filter_map(|(index, (_, line))| Some((scored_similarity(budget, line, anchor)?, index)))
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

/// Whether a line is close enough in length to reach the similarity floor at
/// all, checked in both directions: a much shorter line is as far from the
/// anchor as a much longer one.
fn within_reach(line: &str, anchor: usize, floor: Option<f64>) -> bool {
    let chars = line.chars().count();
    let Some(floor) = floor else {
        return chars <= anchor.saturating_mul(2).max(8);
    };
    let allowed = (1.0 - floor) * as_f64(chars.max(anchor));
    as_f64(chars.abs_diff(anchor)) <= allowed + 1.0
}

/// A file with fewer lines than the block: showing the whole of it is more
/// useful than showing nothing.
fn whole_file(source: &[(usize, String)], wanted: &str, budget: &Budget) -> Option<Candidate> {
    let first = source.first()?.0;
    let last = source.last()?.0;
    Some(Candidate {
        score: scored_similarity(budget, &join_lines(source), wanted)?,
        first,
        last,
        start: 0,
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
/// (FR-TOOL-FILE-2). Answers whether the text changed: a replacement that
/// reproduces the lines it would remove leaves `content` untouched.
fn splice_lines(content: &mut String, first: usize, last: usize, replacement: &str) -> bool {
    let starts = line_starts(content);
    let start = starts.get(first).copied().unwrap_or(content.len());
    let end = starts.get(last + 1).copied().unwrap_or(content.len());
    let removed = content.get(start..end).unwrap_or_default();
    let terminated = removed.ends_with('\n');
    let mut text = replacement.to_owned();
    if terminated && !text.is_empty() && !text.ends_with('\n') {
        text.push_str(eol_at(content, start));
    }
    if removed == text {
        return false;
    }
    content.replace_range(start..end, &text);
    true
}

/// The terminator the line that contains `offset` ends with, and the file's own
/// rule when that line has none at all (an unterminated last line).
///
/// A file with mixed endings is not rewritten to one of them: only the text an
/// edit puts in takes a terminator, and it takes the one the line it lands on
/// already uses, so one stray CRLF elsewhere in the file cannot spread through
/// every inserted line (FR-TOOL-FILE-2).
fn eol_at(content: &str, offset: usize) -> &'static str {
    let Some(position) = content.get(offset..).and_then(|rest| rest.find('\n')) else {
        return eol_of(content);
    };
    let newline = offset + position;
    if content.as_bytes().get(newline.saturating_sub(1)) == Some(&b'\r') {
        "\r\n"
    } else {
        "\n"
    }
}

/// The terminator most of the file's lines use, for the one line an edit lands
/// on that has no terminator of its own to copy.
fn eol_of(content: &str) -> &'static str {
    let crlf = content.matches("\r\n").count();
    let lf = content.matches('\n').count().saturating_sub(crlf);
    if crlf > lf { "\r\n" } else { "\n" }
}

/// Rewrites a replacement's terminators to match the line it lands on, so an
/// edit never mixes endings into a file that uses CRLF.
fn normalize_newlines(text: &str, eol: &str) -> String {
    if eol == "\n" {
        text.to_owned()
    } else {
        text.replace("\r\n", "\n").replace('\n', eol)
    }
}

/// The text that replaces the matched window: the model's own, with its
/// terminators moved onto the file's and its indentation moved onto the file's
/// (FR-TOOL-EDIT-3).
///
/// The move is one prefix for the whole block: whatever the replacement indents
/// a line with that the block also indented its first line with becomes what the
/// file indents that line with, so nested levels keep their depth while the
/// style becomes the file's — a block written in four-space steps lands in a
/// tab-indented file as tabs. A line whose indentation does not begin with the
/// block's is left exactly as the model wrote it: the executor translates the
/// indentation the block lets it translate, and invents nothing.
fn reindent(
    content: &str,
    source: &[(usize, String)],
    window: &Candidate,
    search: &str,
    replacement: &str,
) -> String {
    let eol = eol_at(content, line_offset(content, window.first));
    let replacement = normalize_newlines(replacement, eol);
    match indentation_pair(content, source, window, search) {
        Some((from, to)) if from != to => reindent_text(&replacement, from, to),
        _ => replacement,
    }
}

/// The indentation the block starts at and the one the file gave the same line:
/// what the replacement's indentation is moved between. `None` when the window
/// cannot be lined up with the block at all, which is a file with fewer lines
/// than the block and nothing to move anything onto.
fn indentation_pair<'a>(
    content: &'a str,
    source: &[(usize, String)],
    window: &Candidate,
    search: &'a str,
) -> Option<(&'a str, &'a str)> {
    let lines = line_texts(content);
    let first = *lines.get(source.get(window.start)?.0)?;
    let block = search
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    Some((indentation(block), indentation(first)))
}

/// Moves every line of `text` from one indentation onto another.
fn reindent_text(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for piece in text.split_inclusive('\n') {
        let (body, terminator) = split_terminator(piece);
        if body.trim_start().is_empty() {
            out.push_str(body);
        } else {
            out.push_str(&map_indent(indentation(body), from, to));
            out.push_str(body.trim_start());
        }
        out.push_str(terminator);
    }
    out
}

/// Moves one indentation onto another: every `from` the indentation starts with
/// becomes a `to`, so a line nested three levels deep moves by three. An empty
/// `from` — a block written flush left — moves as a whole.
fn map_indent(indent: &str, from: &str, to: &str) -> String {
    let mut mapped = String::with_capacity(indent.len() + to.len());
    let mut rest = indent;
    if from.is_empty() {
        mapped.push_str(to);
    } else {
        while let Some(after) = rest.strip_prefix(from) {
            mapped.push_str(to);
            rest = after;
        }
    }
    mapped.push_str(rest);
    mapped
}

/// The leading whitespace of a line, which is what indentation is.
fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// A piece of text and the terminator that ends it.
fn split_terminator(piece: &str) -> (&str, &str) {
    match piece.strip_suffix('\n') {
        Some(body) => match body.strip_suffix('\r') {
            Some(body) => (body, "\r\n"),
            None => (body, "\n"),
        },
        None => (piece, ""),
    }
}

/// The byte offset a line starts at.
fn line_offset(content: &str, line: usize) -> usize {
    line_starts(content)
        .get(line)
        .copied()
        .unwrap_or(content.len())
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

/// The similarity of two texts, `1.0` when they are identical.
///
/// The distance is at least the difference in length, which is what lets the
/// caller skip hopeless candidates cheaply.
fn similarity(left: &str, right: &str) -> f64 {
    let distance = levenshtein(left, right);
    let longest = left.chars().count().max(right.chars().count()).max(1);
    1.0 - as_f64(distance) / as_f64(longest)
}

/// The similarity of two texts, when the call can still pay for comparing them:
/// a comparison the budget cannot afford is not made at all, and the window it
/// was for is left out of the report rather than scored approximately.
fn scored_similarity(budget: &Budget, left: &str, right: &str) -> Option<f64> {
    if !budget.charge(left.len().saturating_mul(right.len())) {
        return None;
    }
    Some(similarity(left, right))
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
        lines: shown
            .iter()
            .take(kept)
            .map(|line| shown_line(line))
            .collect(),
        skipped: shown.len().saturating_sub(kept),
    }
}

/// One line as a failure shows it: clipped to a readable width, with a trailing
/// `…` where the width cut it, and with the characters a terminal would act on
/// escaped, so a line in the file cannot hide or reorder the rest of the report
/// (FR-TOOL-EDIT-4).
fn shown_line(line: &str) -> String {
    let (text, _) = clip(line, MAX_LINE_WIDTH);
    escape(&text)
}

fn render(outcome: &EditOutcome, hint: Option<&str>) -> Result<ToolOutput> {
    audit(outcome);
    Ok(ToolOutput {
        parts: vec![Part::text(describe(outcome, hint))],
        structured: Some(serde_json::to_value(outcome)?),
        is_error: outcome.code != EditCode::Applied,
    })
}

fn describe(outcome: &EditOutcome, hint: Option<&str>) -> String {
    use std::fmt::Write as _;

    if outcome.code == EditCode::Applied {
        return applied(outcome);
    }
    let mut text = String::new();

    let Some(failure) = &outcome.failure else {
        if outcome.code == EditCode::Unchanged {
            let _ = writeln!(
                text,
                "edit failed: nothing was written to {}; the {} edit(s) each matched, but \
                 together they leave the file exactly as it was, so the change you meant is \
                 not in any replace",
                outcome.path.display(),
                outcome.matches.len()
            );
        }
        return text;
    };
    let _ = writeln!(
        text,
        "edit failed: nothing was written to {}",
        outcome.path.display()
    );
    if let Some(last) = outcome.matches.len().checked_sub(1) {
        let _ = writeln!(
            text,
            "edits 0-{last} would have applied, so the lines below are the file as they would \
             have left it; the file on disk is untouched"
        );
    } else {
        let _ = writeln!(text, "the lines below are the file as it is");
    }
    match failure.reason {
        EditFailureReason::Ambiguous => {
            let _ = writeln!(text, "{}", ambiguous_report(failure));
        }
        EditFailureReason::NoSuchOccurrence => {
            let _ = writeln!(text, "{}", out_of_range(failure));
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
        EditFailureReason::Unchanged => {
            let _ = writeln!(
                text,
                "edit {} matched, but its replace is exactly what the file already has there, so \
                 it would change nothing; put the new text in replace",
                failure.edit
            );
        }
    }
    if let Some(hint) = hint {
        let _ = writeln!(text, "{hint}");
    }
    // Two candidates of an ambiguous block can render identically — two lines
    // apart are alike in every neighbourhood a report can show — so the heading
    // numbers them the way `occurrence` counts. Without the number the two
    // snippets are a puzzle; with it, the answer to "which one?" is one digit.
    let numbered = failure.reason == EditFailureReason::Ambiguous
        && failure.confidence.is_none()
        && failure.candidates.len() > 1;
    for (index, candidate) in failure.candidates.iter().enumerate() {
        let last = candidate.start_line + candidate.lines.len().saturating_sub(1);
        if numbered {
            let _ = writeln!(
                text,
                "occurrence {} at lines {}-{last}:",
                index + 1,
                candidate.start_line
            );
        } else {
            let _ = writeln!(text, "lines {}-{last}:", candidate.start_line);
        }
        for (offset, line) in candidate.lines.iter().enumerate() {
            let _ = writeln!(text, "{:>5} | {line}", candidate.start_line + offset);
        }
        if candidate.skipped > 0 {
            let _ = writeln!(text, "        … {} more line(s)", candidate.skipped);
        }
    }
    text
}

/// The report for a request that applied.
fn applied(outcome: &EditOutcome) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
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
    text
}

/// The report for a block that matched in more than one place.
///
/// The advice used to be to widen the block, and only that. It is the wrong
/// first answer for the case this hits most: two identical lines a file apart
/// cannot be told apart by any amount of surrounding context, and copying twenty
/// lines to disambiguate something the file already numbers is work the request
/// does not have to do. Naming the occurrence is one number.
fn ambiguous_report(failure: &EditFailure) -> String {
    if failure.confidence.is_some() {
        return format!(
            "edit {} has {} plausible region(s) and no single one is clearly best; widen the \
             block until one is",
            failure.edit, failure.occurrences
        );
    }
    let at_least = if failure.occurrences > MAX_OCCURRENCES {
        "at least "
    } else {
        ""
    };
    format!(
        "edit {} matches {at_least}{} time(s), so it does not say which one to change; set \
         \"occurrence\" to pick one (1 is the first, in file order), or widen the block until \
         only one place matches",
        failure.edit, failure.occurrences
    )
}

/// The report for an `occurrence` the file does not have.
fn out_of_range(failure: &EditFailure) -> String {
    format!(
        "edit {} asks for occurrence {} but the block matches {} time(s); the number counts from \
         1 in file order",
        failure.edit,
        failure.asked_occurrence.unwrap_or(0),
        failure.occurrences
    )
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

/// A retry hint for the most common escaping mistake: a JSON string that was
/// meant to contain a line break but arrived as the two characters `\n` (or the
/// reverse), so a block that looks identical in a chat window does not exist in
/// the file. The hint is text only; matching stays literal and unchanged.
fn escape_hint(search: &str, content: &str) -> Option<String> {
    if let Some(literal) = literal_escape(search) {
        let unescaped = unescape_breaks(search);
        if contains_text(content, &unescaped) {
            return Some(format!(
                "hint: the search contains the two characters `{literal}`, but the file has the \
                 real character there; send it directly. The search is literal text, not a regex."
            ));
        }
    }
    if let Some(real) = real_break(search) {
        let escaped = escape_breaks(search);
        if contains_text(content, &escaped) {
            return Some(format!(
                "hint: the file contains the two characters `{real}` on one line, not the real \
                 character; if you meant that text, send a backslash followed by the letter."
            ));
        }
    }
    None
}

/// The first escaped line break or tab in a block, if the block has one.
fn literal_escape(text: &str) -> Option<&'static str> {
    ["\\n", "\\r", "\\t"]
        .into_iter()
        .find(|needle| text.contains(*needle))
}

/// The first real line break or tab in a block, if the block has one.
fn real_break(text: &str) -> Option<&'static str> {
    [('\n', "\\n"), ('\r', "\\r"), ('\t', "\\t")]
        .into_iter()
        .find_map(|(needle, label)| text.contains(needle).then_some(label))
}

/// Turns the two-character escapes a JSON caller may have meant into the real
/// characters. A backslash before anything else is left exactly as written, so
/// a Windows path is not mistaken for an escape sequence.
fn unescape_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_breaks(text: &str) -> String {
    text.replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Whether `content` has `needle`, treating a CRLF file as also having the LF
/// form so a hint is not lost to a line-ending difference.
fn contains_text(content: &str, needle: &str) -> bool {
    content.contains(needle)
        || (content.contains('\r') && content.replace("\r\n", "\n").contains(needle))
}

/// Records what an edit did where a trail can find it: the versions it worked
/// from and left, and — since a fallback is the one thing here the model did not
/// ask for — how many edits landed without an exact match and how close the
/// loosest of them was.
fn audit(outcome: &EditOutcome) {
    let approximate = outcome
        .matches
        .iter()
        .filter(|entry| entry.kind != MatchKind::Exact)
        .count();
    let loose = outcome
        .matches
        .iter()
        .map(|entry| entry.confidence)
        .reduce(f64::min);
    tracing::info!(
        target: "norte.audit",
        tool = "edit",
        code = ?outcome.code,
        path = %outcome.path.display(),
        written = outcome.written,
        applied = outcome.matches.len(),
        approximate,
        loose_confidence = loose,
        before_digest = %outcome.before_digest,
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
        let directory =
            std::env::temp_dir().join(format!("norte-edit-{name}-{}-{ticket}", std::process::id()));
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
                    occurrence: None,
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
    async fn a_threshold_outside_the_unit_interval_is_read_as_the_strict_end() {
        // 2.5 is a host asking for "stricter than strict"; -0.5 is not a
        // threshold at all. Neither may make an edit looser than the default.
        let strict = EditLimits {
            fuzzy_threshold: Some(2.5),
            ..EditLimits::default()
        };
        let (path, request) = seeded(
            "above",
            SOURCE,
            vec![("pub struct Config {\n    pub port: u16;\n}", "replaced")],
        );
        let output = EditTool::new()
            .with_limits(strict)
            .call(request)
            .await
            .expect("edit");
        assert!(output.is_error, "a content difference stays refused");
        assert_eq!(read(&path), SOURCE);

        let (_, request) = seeded(
            "above-whitespace",
            SOURCE,
            vec![("pub struct Config {\npub port: u16,\n}", "replaced")],
        );
        let output = EditTool::new()
            .with_limits(strict)
            .call(request)
            .await
            .expect("edit");
        assert!(
            !output.is_error,
            "and a whitespace difference still applies: {}",
            describe_failure(&output)
        );

        let nonsense = EditLimits {
            fuzzy_threshold: Some(-0.5),
            ..EditLimits::default()
        };
        let (path, request) = seeded(
            "below",
            SOURCE,
            vec![("pub struct Config {\n    pub port: u16;\n}", "replaced")],
        );
        let output = EditTool::new()
            .with_limits(nonsense)
            .call(request)
            .await
            .expect("edit");
        assert!(
            output.is_error,
            "a nonsense threshold cannot accept a wrong block"
        );
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn a_threshold_of_nan_leaves_only_exact_matches() {
        let limits = EditLimits {
            fuzzy_threshold: Some(f64::NAN),
            ..EditLimits::default()
        };
        let (path, request) = seeded(
            "nan",
            SOURCE,
            vec![("pub struct Config {\npub port: u16,\n}", "replaced")],
        );
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
    async fn a_block_that_differs_in_content_is_refused_by_default() {
        // The default tolerance is whitespace, which is what the tool tells the
        // model: `u16;` where the file has `u16,` is a block the model got
        // wrong, and applying it would write a file neither of them asked for.
        let (path, request) = seeded(
            "strict",
            SOURCE,
            vec![(
                "pub struct Config {\n    pub port: u16;\n}",
                "pub struct Config {\n    pub port: u32;\n}",
            )],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "no_match");
        let confidence = structured["failure"]["confidence"]
            .as_f64()
            .expect("confidence");
        assert!(
            (0.95..1.0).contains(&confidence),
            "the block is close, and still refused, got {confidence}"
        );
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn a_host_may_allow_a_block_that_is_close_in_content() {
        let limits = EditLimits {
            fuzzy_threshold: Some(0.95),
            ..EditLimits::default()
        };
        let (path, request) = seeded(
            "fuzzy",
            SOURCE,
            vec![(
                "pub struct Config {\n    pub port: u16;\n}",
                "pub struct Config {\n    pub port: u32,\n}",
            )],
        );
        let output = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect("edit");
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
    async fn a_failure_shows_a_region_once() {
        // Two lines: every candidate window lands on the same region, and the
        // model is shown that region once instead of once per window.
        let (path, request) = seeded(
            "short",
            "alpha\nbeta\n",
            vec![("a line that is not there", "anything")],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "no_match");
        assert_eq!(
            structured["failure"]["candidates"].as_array().map(Vec::len),
            Some(1)
        );
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert_eq!(text.matches("lines 1-2:").count(), 1, "{text}");
        assert_eq!(read(&path), "alpha\nbeta\n");
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
        // The message says how to resolve it, not only that it is unresolved:
        // widening the block is impossible for two identical lines, and the
        // occurrence is one number.
        assert!(text.contains("matches 2 time(s)"), "{text}");
        assert!(
            text.contains("occurrence"),
            "the answer is in the message: {text}"
        );
        assert!(text.contains("occurrence"), "{text}");
        assert!(
            text.contains("    3 |") && text.contains("   13 |"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn same_line_occurrences_are_counted_separately() {
        let (path, request) = seeded("same-line", "bar bar\n", vec![("bar", "baz")]);
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["occurrences"], 2);
        let text = describe_failure(&output);
        assert!(text.contains("matches 2 time(s)"), "{text}");
        assert!(
            text.contains("occurrence"),
            "the answer is in the message: {text}"
        );
        assert_eq!(read(&path), "bar bar\n");
    }

    #[tokio::test]
    async fn a_literal_escape_is_called_out_when_the_file_has_a_line_break() {
        let (path, request) = seeded(
            "escape-literal",
            "alpha\nbeta\n",
            vec![(r"alpha\nbeta", "gamma")],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "no_match");
        let text = describe_failure(&output);
        assert!(text.contains("hint:"), "{text}");
        assert!(text.contains("two characters `\\n`"), "{text}");
        assert!(text.contains("send it directly"), "{text}");
        assert_eq!(read(&path), "alpha\nbeta\n");
    }

    #[tokio::test]
    async fn a_real_line_break_is_called_out_when_the_file_has_a_literal_escape() {
        let (path, request) = seeded(
            "escape-real",
            "alpha\\nbeta\n",
            vec![("alpha\nbeta", "gamma")],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "no_match");
        let text = describe_failure(&output);
        assert!(text.contains("hint:"), "{text}");
        assert!(text.contains("two characters `\\n` on one line"), "{text}");
        assert_eq!(read(&path), "alpha\\nbeta\n");
    }

    #[tokio::test]
    async fn a_literal_escape_that_matches_applies_without_a_hint() {
        let (path, request) = seeded(
            "escape-exact",
            "alpha\\nbeta\n",
            vec![(r"alpha\nbeta", "gamma")],
        );
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&path), "gamma\n");
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
        let text = describe_failure(&output);
        assert!(text.contains("plausible region(s)"), "{text}");
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
        let limits = EditLimits {
            fuzzy_threshold: Some(0.95),
            ..EditLimits::default()
        };
        let output = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect("edit");
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
                occurrence: None,
            }],
        };
        let error = EditTool::new().call(request).await.expect_err("missing");
        assert!(error.to_string().contains("absent.rs"));
    }

    #[tokio::test]
    async fn an_empty_search_is_invalid_input() {
        let (_, request) = seeded("blank", SOURCE, vec![("", "replacement")]);
        let error = EditTool::new().call(request).await.expect_err("blank");
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("empty search")));
    }

    #[tokio::test]
    async fn a_blank_search_collapses_a_blank_line_when_it_is_unique() {
        // A search of nothing but whitespace is not an empty search: it is a
        // real block, and uniqueness is what decides whether it may apply.
        let (path, request) = seeded("collapse", "alpha\n\nbeta\n", vec![("\n\n", "\n")]);
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(structured(&output)["matches"][0]["kind"], "exact");
        assert_eq!(read(&path), "alpha\nbeta\n");
    }

    #[tokio::test]
    async fn a_blank_search_that_is_everywhere_is_ambiguous() {
        let (path, request) = seeded("collapse-ambiguous", SOURCE, vec![("\n\n", "\n")]);
        let output = run(&request).await;
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "ambiguous");
        assert_eq!(read(&path), SOURCE);
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
        let mut registry = norte_core::ToolRegistry::new();
        registry.register_tool(&EditTool::new()).expect("register");
        let name = norte_core::ToolName::new("edit").expect("name");
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(spec.description, <EditTool as ToolMetadata>::DESCRIPTION);

        let path = scratch("envelope").join("main.rs");
        std::fs::write(&path, "let a = 1;\n").expect("seed");
        let output = registry
            .execute(
                &norte_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": path,
                    "edits": [{ "search": "let a = 1;", "replace": "let a = 2;" }]
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        assert!(!output.parts.is_empty(), "a completed call reports output");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "let a = 2;\n"
        );
    }

    #[tokio::test]
    async fn unknown_arguments_are_rejected_before_the_body_runs() {
        let mut registry = norte_core::ToolRegistry::new();
        registry.register_tool(&EditTool::new()).expect("register");
        let name = norte_core::ToolName::new("edit").expect("name");
        let error = registry
            .execute(
                &norte_core::ToolContext::new(),
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

    #[tokio::test]
    async fn overlapping_matches_are_two_places() {
        // `match_indices` would call this one place, and an edit that applies
        // where the block is not is worse than one that refuses.
        let (path, request) = seeded("overlap", "aaa\n", vec![("aa", "b")]);
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["occurrences"], 2);
        assert_eq!(read(&path), "aaa\n");
    }

    #[tokio::test]
    async fn more_places_than_the_cap_is_ambiguous_not_a_miss() {
        // Seventeen lines that all match up to whitespace: the block is not
        // unique, and a report of "no match, 100% similar" would be a lie.
        let mut content = String::new();
        for _ in 0..=MAX_ANCHORS {
            content.push_str("fn one() {\n  let a = 1;\n}\n\n");
        }
        let (path, request) = seeded(
            "anchors",
            &content,
            vec![("fn one() {\n    let a = 1;\n}", "fn two() {}")],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        let occurrences = structured["failure"]["occurrences"]
            .as_u64()
            .expect("occurrences");
        assert!(
            occurrences > MAX_ANCHORS as u64,
            "the count is the number of places, not the number scored: {occurrences}"
        );
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn a_fallback_that_is_not_clearly_best_is_ambiguous() {
        // Two blocks within the margin of each other, and the host has allowed
        // a content match: picking one of them would be a coin toss.
        let content =
            "fn one() {\n    let value = 100;\n}\n\nfn one() {\n    let value = 102;\n}\n";
        let (path, request) = seeded(
            "margin",
            content,
            vec![(
                "fn one() {\n    let value = 101;\n}",
                "fn one() {\n    let value = 103;\n}",
            )],
        );
        let limits = EditLimits {
            fuzzy_threshold: Some(0.8),
            ..EditLimits::default()
        };
        let output = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect("edit");
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "ambiguous");
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn a_block_that_matches_up_to_whitespace_lands_in_the_file_indentation() {
        // The block is written in four-space steps and the file indents with
        // tabs: the replacement moves onto the file's indentation level by
        // level, and the lines around it are left alone (FR-TOOL-EDIT-3).
        let content = "impl Config {\n\tfn port(&self) -> u16 {\n\t\tself.port\n\t}\n}\n";
        let (path, request) = seeded(
            "style",
            content,
            vec![(
                "    fn port(&self) -> u16 {\n        self.port\n    }",
                "    fn port(&self) -> u32 {\n        self.port as u32\n    }",
            )],
        );
        let output = run(&request).await;
        assert!(!output.is_error, "{}", describe_failure(&output));
        assert_eq!(
            read(&path),
            "impl Config {\n\tfn port(&self) -> u32 {\n\t\tself.port as u32\n\t}\n}\n"
        );
        assert_eq!(structured(&output)["matches"][0]["kind"], "whitespace");
    }

    #[tokio::test]
    async fn a_search_that_ends_in_a_newline_is_replaced_literally() {
        // The exact path replaces bytes: a block that carries the terminator
        // takes it with it, and the model asked for exactly that.
        let (path, request) = seeded("literal", "alpha\nbeta\n", vec![("alpha\n", "one")]);
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&path), "onebeta\n");
    }

    #[tokio::test]
    async fn a_replaced_block_keeps_the_terminator_the_following_line_needs() {
        // The line-based path puts back the terminator the replaced lines ended
        // with, so the line after them stays whole (FR-TOOL-FILE-2).
        let content = "fn a() {\n    x\n}\nfn b() {}\n";
        let (path, request) = seeded(
            "terminated",
            content,
            vec![("fn a() {\nx\n}", "fn a() {\ny\n}")],
        );
        let output = run(&request).await;
        assert!(!output.is_error, "{}", describe_failure(&output));
        assert_eq!(read(&path), "fn a() {\ny\n}\nfn b() {}\n");
    }

    #[tokio::test]
    async fn only_the_edited_line_takes_its_own_terminator() {
        // One stray CRLF does not spread: the replacement takes the terminator
        // of the line it lands on (FR-TOOL-FILE-2).
        let content = "alpha\r\nbeta\ngamma\n";
        let (path, request) = seeded("mixed", content, vec![("beta", "delta")]);
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(
            read(&path),
            "alpha\r\nbeta\ngamma\n".replace("beta", "delta")
        );
    }

    #[tokio::test]
    async fn an_ambiguous_failure_numbers_its_occurrences() {
        // Two identical lines a file apart render the same in any neighbourhood,
        // so a report that shows both without numbering them is a puzzle. The
        // numbers are the ones `occurrence` counts with, so the heading and the
        // field are the same answer said twice.
        let content = "let a = 1;\nlet b = 2;\nlet a = 1;\n";
        let (path, request) = seeded("numbered", content, vec![("let a = 1;", "let a = 3;")]);
        let output = run(&request).await;
        assert!(output.is_error);
        let text = describe_failure(&output);
        assert!(
            text.contains("occurrence 1 at lines"),
            "the first is numbered: {text}"
        );
        assert!(
            text.contains("occurrence 2 at lines"),
            "and so is the second: {text}"
        );
        // The numbers are what the answer uses, so the message says so.
        assert!(text.contains("set \"occurrence\""), "{text}");
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn a_unique_failure_is_not_numbered() {
        // One candidate is not a choice: numbering it would suggest there are
        // others, and the field it names has nothing to pick.
        let (path, request) = seeded(
            "not-numbered",
            SOURCE,
            vec![("pub port: u17,", "pub port: u32,")],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        let text = describe_failure(&output);
        assert!(
            !text.contains("occurrence 1"),
            "a single candidate is not a list: {text}"
        );
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn an_occurrence_picks_one_of_several_identical_places() {
        // The case the ambiguity failure hits most, and the answer to it. Two
        // lines two apart are identical in every neighbourhood a report could
        // show — copying twenty lines to disambiguate something the file already
        // numbers is work the request does not have to do. The number is the
        // whole of it.
        let content = "let a = 1;\nlet b = 2;\nlet a = 1;\n";
        let path = scratch("occurrence").join("main.rs");
        std::fs::write(&path, content).expect("seed");
        let request = EditRequest {
            path: path.clone(),
            edits: vec![EditSpec::new("let a = 1;", "let a = 3;").nth(2)],
        };
        let output = run(&request).await;
        assert!(!output.is_error, "the second one was found: {output:?}");
        assert_eq!(
            read(&path),
            "let a = 1;\nlet b = 2;\nlet a = 3;\n",
            "the second occurrence changed and the first did not"
        );

        // The first one, in the same file and the same block: the other way.
        let path = scratch("occurrence-first").join("main.rs");
        std::fs::write(&path, content).expect("seed");
        let request = EditRequest {
            path: path.clone(),
            edits: vec![EditSpec::new("let a = 1;", "let a = 3;").nth(1)],
        };
        let output = run(&request).await;
        assert!(!output.is_error, "{output:?}");
        assert_eq!(read(&path), "let a = 3;\nlet b = 2;\nlet a = 1;\n");
    }

    #[tokio::test]
    async fn an_occurrence_the_file_does_not_have_is_refused_with_the_count() {
        // A number past the end is a mistake, and the useful part of saying so is
        // the count: "there are two" is what lets the next attempt be right.
        let content = "let a = 1;\nlet b = 2;\nlet a = 1;\n";
        let (path, request) = seeded(
            "occurrence-past",
            content,
            vec![("let a = 1;", "let a = 3;")],
        );
        let mut request = request;
        request.edits[0] = request.edits[0].clone().nth(3);
        // A failure to match is a reported outcome, not a rejected request: the
        // call answers, and the answer says what went wrong.
        let output = run(&request).await;
        assert!(output.is_error, "the third of two does not exist");
        let text: String = output
            .parts
            .iter()
            .filter_map(norte_core::Part::as_text)
            .collect();
        assert!(text.contains("occurrence 3"), "{text}");
        assert!(
            text.contains("matches 2 time(s)"),
            "and how many there are: {text}"
        );
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["reason"], "no_such_occurrence");
        assert_eq!(structured["failure"]["asked_occurrence"], 3);
        assert_eq!(read(&path), content, "nothing was written");
    }

    #[tokio::test]
    async fn an_occurrence_of_zero_is_invalid_input() {
        // Counting starts at one, as the file numbers it: a zero is a request
        // that meant "the first" and said something else.
        let (path, mut request) = seeded(
            "occurrence-zero",
            SOURCE,
            vec![("pub port: u16,", "pub port: u32,")],
        );
        request.edits[0] = request.edits[0].clone().nth(0);
        let error = EditTool::new().call(request).await.expect_err("zero");
        assert!(
            matches!(&error, Error::InvalidInput(reason) if reason.contains("counted from 1")),
            "{error}"
        );
        assert_eq!(read(&path), SOURCE, "nothing was written");
    }

    #[tokio::test]
    async fn an_occurrence_on_a_unique_block_still_applies() {
        // It is not only for the ambiguous case: a request that names the first
        // of one is not wrong, and refusing it would make the number a trap.
        let (path, mut request) = seeded(
            "occurrence-first-of-one",
            SOURCE,
            vec![("pub port: u16,", "pub port: u32,")],
        );
        request.edits[0] = request.edits[0].clone().nth(1);
        let output = run(&request).await;
        assert!(!output.is_error, "{output:?}");
        assert!(read(&path).contains("pub port: u32,"));
    }

    #[tokio::test]
    async fn an_ambiguous_failure_offers_the_occurrence() {
        // The advice used to be to widen the block, and only that. For two
        // identical lines it is advice that cannot be taken: no neighbourhood
        // tells them apart. The message says what can be done.
        let content = "let a = 1;\nlet b = 2;\nlet a = 1;\n";
        let (path, request) = seeded(
            "ambiguous-advice",
            content,
            vec![("let a = 1;", "let a = 3;")],
        );
        let output = run(&request).await;
        assert!(output.is_error, "an ambiguous block is refused");
        let structured = structured(&output);
        assert_eq!(structured["code"], "ambiguous");
        assert_eq!(structured["failure"]["occurrences"], 2);
        let text = output
            .parts
            .iter()
            .filter_map(norte_core::Part::as_text)
            .collect::<String>();
        assert!(
            text.contains("occurrence"),
            "the failure says how to pick one: {text}"
        );
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn an_identical_search_and_replace_is_invalid_input() {
        let (path, request) = seeded("noop", SOURCE, vec![("pub port: u16,", "pub port: u16,")]);
        let modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("modified");
        let error = EditTool::new().call(request).await.expect_err("a no-op");
        assert!(
            matches!(&error, Error::InvalidInput(reason) if reason.contains("edit 0") && reason.contains("same text")),
            "{error}"
        );
        assert_eq!(read(&path), SOURCE);
        let after = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("modified");
        assert_eq!(modified, after, "the file was not rewritten");
    }

    #[tokio::test]
    async fn a_no_op_in_a_batch_fails_the_whole_batch() {
        // The case a model hits most: one real edit and one whose replace it
        // forgot to change. Reporting both as applied hides the forgotten one.
        let (path, request) = seeded(
            "noop-batch",
            SOURCE,
            vec![
                ("pub port: u16,", "pub port: u32,"),
                ("println!(\"{config:?}\");", "println!(\"{config:?}\");"),
            ],
        );
        let error = EditTool::new().call(request).await.expect_err("a no-op");
        assert!(
            matches!(&error, Error::InvalidInput(reason) if reason.contains("edit 1")),
            "{error}"
        );
        assert_eq!(read(&path), SOURCE, "the real edit is not written either");
    }

    #[tokio::test]
    async fn a_replace_that_only_differs_in_line_endings_changes_nothing() {
        // search and replace differ as strings, but the replacement takes the
        // file's CRLF and lands as the very text it replaces.
        let content = "one\r\ntwo\r\nthree\r\n";
        let (path, request) = seeded("noop-crlf", content, vec![("two\r\nthree", "two\nthree")]);
        let output = run(&request).await;
        assert!(output.is_error, "a no-op is a failure");
        let structured = structured(&output);
        assert_eq!(structured["code"], "unchanged");
        assert_eq!(structured["written"], false);
        assert_eq!(structured["failure"]["reason"], "unchanged");
        assert_eq!(structured["failure"]["edit"], 0);
        assert_eq!(structured["failure"]["candidates"][0]["start_line"], 1);
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(text.contains("would change nothing"), "{text}");
        assert!(!text.contains("applied"), "{text}");
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn a_whitespace_match_whose_replace_is_already_there_changes_nothing() {
        let content = "fn main() {\n    let  x = 1;\n}\n";
        let (path, request) = seeded(
            "noop-whitespace",
            content,
            vec![("    let x = 1;", "    let  x = 1;")],
        );
        let output = run(&request).await;
        assert!(output.is_error, "a no-op is a failure");
        let structured = structured(&output);
        assert_eq!(structured["code"], "unchanged");
        assert_eq!(structured["failure"]["reason"], "unchanged");
        assert_eq!(structured["failure"]["confidence"], 1.0);
        assert_eq!(read(&path), content);
    }

    #[tokio::test]
    async fn edits_that_cancel_each_other_out_are_unchanged() {
        let (path, request) = seeded(
            "noop-cancel",
            SOURCE,
            vec![
                ("pub port: u16,", "pub port: u32,"),
                ("pub port: u32,", "pub port: u16,"),
            ],
        );
        let output = run(&request).await;
        assert!(output.is_error, "a no-op is a failure");
        let structured = structured(&output);
        assert_eq!(structured["code"], "unchanged");
        assert_eq!(structured["written"], false);
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(text.contains("edit failed"), "{text}");
        assert!(text.contains("exactly as it was"), "{text}");
        assert_eq!(read(&path), SOURCE);
    }

    #[test]
    fn the_schema_tells_the_model_that_search_and_replace_must_differ() {
        let schema = serde_json::to_string(&schemars::schema_for!(EditRequest)).expect("schema");
        assert!(schema.contains("It must differ from search"), "{schema}");
        assert!(EditTool::DESCRIPTION.contains("has to differ"));
        assert!(!EditTool::DESCRIPTION.contains("reported as unchanged"));
    }

    #[tokio::test]
    async fn the_digest_of_the_version_an_edit_replaced_is_reported() {
        let (path, request) = seeded("before", SOURCE, vec![("pub port: u16,", "pub port: u32,")]);
        let digest = Digest::of(SOURCE.as_bytes());
        let output = run(&request).await;
        let structured = structured(&output);
        let before = structured["before_digest"].as_str().expect("before_digest");
        let after = structured["content_digest"]
            .as_str()
            .expect("content_digest");
        assert_eq!(before, digest.to_string());
        assert_ne!(before, after);
        assert_ne!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn a_change_between_the_read_and_the_write_is_refused() {
        let directory = scratch("conflict");
        let path = directory.join("main.rs");
        std::fs::write(&path, "one\n").expect("seed");
        let digest = Digest::of(b"one\n");
        std::fs::write(&path, "another writer was here\n").expect("write");
        let error = ensure_unchanged(&path, &digest, 1024).expect_err("a changed file");
        assert!(
            error.to_string().contains("changed while the edit"),
            "{error}"
        );
        assert_eq!(read(&path), "another writer was here\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_through_a_symlink_keeps_the_link_and_changes_its_target() {
        let directory = scratch("symlink");
        let target = directory.join("real.rs");
        std::fs::write(&target, SOURCE).expect("seed");
        let link = directory.join("link.rs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let request = EditRequest {
            path: link.clone(),
            edits: vec![EditSpec {
                search: "pub port: u16,".to_owned(),
                replace: "pub port: u32,".to_owned(),
                occurrence: None,
            }],
        };
        let output = run(&request).await;
        assert!(!output.is_error);
        assert_eq!(read(&target), SOURCE.replace("u16", "u32"));
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("metadata")
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
    }

    #[tokio::test]
    async fn control_characters_in_the_file_are_escaped_in_the_report() {
        let content = "let a = 1;\rlet b = 2;\u{202e}hidden\n";
        let (_, request) = seeded("escape", content, vec![("nothing like this", "x")]);
        let output = run(&request).await;
        assert!(output.is_error);
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(
            text.contains("\\r"),
            "a lone carriage return is shown: {text}"
        );
        assert!(
            text.contains("\\u{202e}"),
            "a bidi override is shown: {text}"
        );
        assert!(
            !text.contains('\u{202e}') && !text.contains('\r'),
            "no control character travels raw: {text:?}"
        );
    }

    #[tokio::test]
    async fn a_block_over_the_limit_is_refused_before_the_file_is_read() {
        let search = "x".repeat(EditLimits::default().max_block_bytes + 1);
        let (_, request) = seeded("bigblock", SOURCE, vec![(&search, "y")]);
        let error = EditTool::new().call(request).await.expect_err("big block");
        assert!(
            matches!(&error, Error::InvalidInput(reason) if reason.contains("over the limit")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn more_edits_than_the_limit_are_refused_before_the_file_is_read() {
        let limits = EditLimits {
            max_edits: 1,
            ..EditLimits::default()
        };
        let (path, request) = seeded(
            "manyedits",
            SOURCE,
            vec![
                ("pub port: u16,", "pub port: u32,"),
                ("use std::fmt;", "use std::io;"),
            ],
        );
        let error = EditTool::new()
            .with_limits(limits)
            .call(request)
            .await
            .expect_err("too many edits");
        assert!(
            matches!(&error, Error::InvalidInput(reason) if reason.contains("over the limit")),
            "{error}"
        );
        assert_eq!(read(&path), SOURCE);
    }

    #[tokio::test]
    async fn a_block_that_belongs_nowhere_cannot_hold_the_executor() {
        use std::fmt::Write as _;

        // A block of a size a model could send, against a file of the largest
        // size accepted, with nothing in common: the comparison budget has to
        // end it long before the runtime is held for minutes (FR-TOOL-EDIT-3).
        let mut content = String::new();
        for number in 0..20_000 {
            let _ = writeln!(content, "let value_{number} = {number};");
        }
        let mut search = String::new();
        for number in 0..500 {
            let _ = writeln!(search, "call_the_function(number_{number});");
        }
        let (_, request) = seeded("budget", &content, vec![(&search, "replacement")]);
        let started = std::time::Instant::now();
        let output = run(&request).await;
        let elapsed = started.elapsed();
        assert!(output.is_error);
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the fallback held the runtime for {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_after_some_edits_says_which_copy_it_shows() {
        let (path, request) = seeded(
            "partial",
            SOURCE,
            vec![
                ("use std::fmt;", "use std::io;"),
                ("pub port: u16,", "pub port: u32,"),
                ("this line is not there", "replacement"),
            ],
        );
        let output = run(&request).await;
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "no_match");
        assert_eq!(structured["failure"]["edit"], 2);
        assert_eq!(
            structured["matches"].as_array().map(Vec::len),
            Some(2),
            "the edits before it are reported as applied to the copy"
        );
        let text = output.parts.first().and_then(Part::as_text).expect("text");
        assert!(
            text.contains("edits 0-1 would have applied"),
            "the report says which copy it shows: {text}"
        );
        assert_eq!(read(&path), SOURCE, "the file on disk is untouched");
    }

    /// The text of a failed result, for an assertion that has to explain itself.
    fn describe_failure(output: &ToolOutput) -> String {
        output
            .parts
            .first()
            .and_then(Part::as_text)
            .unwrap_or_default()
            .to_owned()
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
