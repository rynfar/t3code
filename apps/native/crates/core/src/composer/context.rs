//! Composer context the prompt links inline: terminal excerpts added from the terminal drawer,
//! and the link edits every context kind shares. Ports `web/src/lib/terminalContext.ts`,
//! `terminalContextReference` and `terminalContextRecord` from
//! `web/src/lib/composerContextRecords.ts`, `normalizeTerminalContextForThread` and
//! `terminalContextDedupKey` from `web/src/composerDraftStore.ts`, the link edits of
//! `web/src/lib/composerContextReferences.ts`, `formatComposerContextReference` from
//! `shared/src/composerContextReferences.ts`, `terminalSelectionLineRange` from
//! `web/src/components/ThreadTerminalDrawer.tsx`, and `deriveComposerSendState`,
//! `snapshotComposerSend` and `buildExpiredTerminalContextToastCopy` from
//! `web/src/components/ChatView.logic.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::menu::{CONTEXT_HREF_PREFIX, context_references, kind_scoped_context_id, sanitize_context_label, to_context_id};

pub const TERMINAL_KIND: &str = "terminal";

/// `INLINE_TERMINAL_CONTEXT_PLACEHOLDER`: where drafts written before inline links kept a
/// terminal excerpt.
pub const LEGACY_TERMINAL_PLACEHOLDER: char = '\u{FFFC}';

/// `formatComposerContextReference`: the inline link a prompt carries for one context.
pub fn format_reference(kind: &str, context_id: &str, label: &str) -> String {
    let label = sanitize_context_label(label, kind);
    let bang = if kind == "image" { "!" } else { "" };
    format!("{bang}[{label}]({CONTEXT_HREF_PREFIX}{kind}/{context_id})")
}

/// What the terminal hands over when the user adds a selection (`TerminalContextSelection`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSelection {
    pub terminal_id: String,
    pub terminal_label: String,
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

/// `terminalSelectionLineRange`: 1-based lines from the selection's first and last screen rows,
/// counted from the top of the scrollback.
pub fn selection_line_range(start_row: u32, end_row: u32) -> (u32, u32) {
    let line_start = start_row + 1;
    (line_start, line_start.max(end_row + 1))
}

/// A terminal excerpt in a draft (`TerminalContextDraft`), in the shape the web persists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalContext {
    pub id: String,
    pub thread_id: String,
    pub created_at: String,
    pub terminal_id: String,
    pub terminal_label: String,
    pub line_start: u32,
    pub line_end: u32,
    /// Absent in drafts saved before excerpts kept their text; such an excerpt has expired.
    #[serde(default)]
    pub text: String,
}

/// `normalizeTerminalContextText`: CRLF becomes LF and blank lines around the excerpt go.
pub fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_matches('\n').to_owned()
}

impl TerminalContext {
    /// `normalizeTerminalContextForThread`: `None` without a terminal id or label.
    pub fn new(selection: TerminalSelection, id: &str, thread_id: &str, created_at: &str) -> Option<TerminalContext> {
        let terminal_id = selection.terminal_id.trim();
        let terminal_label = selection.terminal_label.trim();
        if terminal_id.is_empty() || terminal_label.is_empty() {
            return None;
        }
        let line_start = selection.line_start.max(1);
        Some(TerminalContext {
            id: to_context_id(id),
            thread_id: thread_id.to_owned(),
            created_at: created_at.to_owned(),
            terminal_id: terminal_id.to_owned(),
            terminal_label: terminal_label.to_owned(),
            line_start,
            line_end: selection.line_end.max(line_start),
            text: normalize_text(&selection.text),
        })
    }

    /// `formatTerminalContextLabel`: `Terminal 1 line 4` or `Terminal 1 lines 4-9`.
    pub fn label(&self) -> String {
        format!("{} {}", self.terminal_label, self.range_label())
    }

    fn range_label(&self) -> String {
        if self.line_start == self.line_end {
            format!("line {}", self.line_start)
        } else {
            format!("lines {}-{}", self.line_start, self.line_end)
        }
    }

    /// The popover's header: `Line 4` or `Lines 4–9`.
    pub fn line_caption(&self) -> String {
        if self.line_start == self.line_end {
            format!("Line {}", self.line_start)
        } else {
            format!("Lines {}–{}", self.line_start, self.line_end)
        }
    }

    pub fn context_id(&self) -> String {
        kind_scoped_context_id(TERMINAL_KIND, &self.id)
    }

    /// `formatTerminalContextReference`.
    pub fn reference(&self) -> String {
        format_reference(TERMINAL_KIND, &self.context_id(), &self.label())
    }

    /// `isTerminalContextExpired`: nothing left to send.
    pub fn is_expired(&self) -> bool {
        normalize_text(&self.text).is_empty()
    }

    /// `terminalContextDedupKey`: the same lines of the same terminal are added once.
    fn same_lines(&self, other: &TerminalContext) -> bool {
        self.terminal_id == other.terminal_id && self.line_start == other.line_start && self.line_end == other.line_end
    }

    /// `terminalContextRecord`: the `TerminalContextRecord` a message carries.
    pub fn record(&self) -> Value {
        json!({
            "version": 1,
            "contextId": self.context_id(),
            "kind": TERMINAL_KIND,
            "label": sanitize_context_label(&self.label(), TERMINAL_KIND),
            "terminalId": self.terminal_id,
            "terminalLabel": self.terminal_label,
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
            "text": normalize_text(&self.text),
        })
    }
}

/// The excerpts a prompt still links, in the order it first links them.
pub fn referenced<'a>(prompt: &str, contexts: &'a [TerminalContext]) -> Vec<&'a TerminalContext> {
    let mut found: Vec<&TerminalContext> = Vec::new();
    for reference in context_references(prompt).into_iter().filter(|reference| reference.kind == TERMINAL_KIND) {
        if let Some(context) = contexts.iter().find(|context| context.context_id() == reference.context_id)
            && !found.iter().any(|seen| seen.id == context.id)
        {
            found.push(context);
        }
    }
    found
}

fn is_boundary_whitespace(c: Option<char>) -> bool {
    matches!(c, None | Some(' ' | '\n' | '\t' | '\r'))
}

/// A replacement of `start..end` in the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl ReferenceEdit {
    /// The caret after the inserted text.
    pub fn cursor(&self) -> usize {
        self.start + self.text.len()
    }

    pub fn apply(&self, prompt: &str) -> String {
        format!("{}{}{}", &prompt[..self.start], self.text, &prompt[self.end..])
    }
}

/// `inlineContextReferenceReplacement` at a caret: the link goes after a space unless one is
/// already there, followed by a space that absorbs one already after the caret.
pub fn reference_edit(prompt: &str, cursor: usize, reference: &str) -> ReferenceEdit {
    let mut start = cursor.min(prompt.len());
    while !prompt.is_char_boundary(start) {
        start -= 1;
    }
    let leading = if is_boundary_whitespace(prompt[..start].chars().next_back()) { "" } else { " " };
    let end = if prompt[start..].starts_with(' ') { start + 1 } else { start };
    ReferenceEdit { start, end, text: format!("{leading}{reference} ") }
}

/// `insertInlineContextReference`: the prompt and the caret after the link.
pub fn insert_reference(prompt: &str, cursor: usize, reference: &str) -> (String, usize) {
    let edit = reference_edit(prompt, cursor, reference);
    (edit.apply(prompt), edit.cursor())
}

/// `ChatComposer.addTerminalContext` with `insertComposerDraftTerminalContext`: the edit that
/// links `context` at `cursor`, or `None` when the prompt already links the same lines of the
/// same terminal.
pub fn add_terminal_context(prompt: &str, cursor: usize, contexts: &[TerminalContext], context: &TerminalContext) -> Option<ReferenceEdit> {
    let linked = referenced(prompt, contexts);
    if linked.iter().any(|existing| existing.id == context.id || existing.same_lines(context)) {
        return None;
    }
    Some(reference_edit(prompt, cursor, &context.reference()))
}

/// `removeInlineContextReference`: every link to `context_id` goes with one space beside it.
/// Returns the prompt and where the caret lands.
pub fn remove_reference(prompt: &str, context_id: &str) -> (String, usize) {
    let ranges: Vec<(usize, usize)> = context_references(prompt)
        .into_iter()
        .filter(|reference| reference.context_id == context_id)
        .map(|reference| (reference.start, reference.end))
        .collect();
    if ranges.is_empty() {
        return (prompt.to_owned(), prompt.len());
    }
    let mut result = prompt.to_owned();
    let mut cursor = result.len();
    for (mut start, mut end) in ranges.into_iter().rev() {
        if result[end..].starts_with(' ') {
            end += 1;
        } else if result[..start].ends_with(' ') {
            start -= 1;
        }
        result.replace_range(start..end, "");
        cursor = start;
    }
    if cursor >= result.len() {
        result.truncate(result.trim_end().len());
        cursor = result.len();
    }
    (result, cursor)
}

/// `stripInlineContextReferences`: the prose without any link.
pub fn strip_references(prompt: &str) -> String {
    let mut prose = String::new();
    let mut cursor = 0;
    for reference in context_references(prompt) {
        prose.push_str(&prompt[cursor..reference.start]);
        cursor = reference.end;
    }
    prose.push_str(&prompt[cursor..]);
    prose
}

/// `migrateLegacyTerminalContextPlaceholders`: each placeholder becomes the link of the excerpt
/// in its position; placeholders past the last excerpt are dropped.
pub fn migrate_legacy_placeholders(prompt: &str, contexts: &[TerminalContext]) -> String {
    if !prompt.contains(LEGACY_TERMINAL_PLACEHOLDER) {
        return prompt.to_owned();
    }
    let mut contexts = contexts.iter();
    let mut migrated = String::with_capacity(prompt.len());
    for c in prompt.chars() {
        if c == LEGACY_TERMINAL_PLACEHOLDER {
            if let Some(context) = contexts.next() {
                migrated.push_str(&context.reference());
            }
        } else {
            migrated.push(c);
        }
    }
    migrated
}

/// What a send carries of the prompt's terminal excerpts (`deriveComposerSendState` and
/// `snapshotComposerSend`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSend {
    /// The message text: the prompt without links to expired excerpts, trimmed.
    pub text: String,
    /// The linked excerpts that still have text.
    pub terminals: Vec<TerminalContext>,
    /// Linked excerpts dropped because their text is gone.
    pub expired: usize,
    /// The prompt has words besides its links.
    pub prose: bool,
}

impl TerminalSend {
    pub fn of(prompt: &str, contexts: &[TerminalContext]) -> TerminalSend {
        let linked = referenced(prompt, contexts);
        let (expired, terminals): (Vec<&TerminalContext>, Vec<&TerminalContext>) = linked.into_iter().partition(|context| context.is_expired());
        let text = expired.iter().fold(prompt.to_owned(), |text, context| remove_reference(&text, &context.context_id()).0);
        TerminalSend {
            text: text.trim().to_owned(),
            terminals: terminals.into_iter().cloned().collect(),
            expired: expired.len(),
            prose: !strip_references(prompt).trim().is_empty(),
        }
    }

    /// `hasSendableContent`, given the attachments and the other contexts the prompt links.
    pub fn has_content(&self, attachments: usize, other_contexts: usize) -> bool {
        self.prose || attachments > 0 || !self.terminals.is_empty() || other_contexts > 0
    }
}

/// Which toast `buildExpiredTerminalContextToastCopy` words: the message went without the
/// excerpts, or nothing went at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredNotice {
    Omitted,
    Empty,
}

/// `buildExpiredTerminalContextToastCopy`: title and description.
pub fn expired_toast_copy(count: usize, notice: ExpiredNotice) -> (String, String) {
    let count = count.max(1);
    let noun = if count == 1 { "Expired terminal context" } else { "Expired terminal contexts" };
    let pronoun = if count == 1 { "it" } else { "them" };
    match notice {
        ExpiredNotice::Empty => {
            (format!("{noun} won't be sent"), format!("Remove {pronoun} or re-add {pronoun} to include terminal output."))
        }
        ExpiredNotice::Omitted => {
            (format!("{noun} omitted from message"), format!("Re-add {pronoun} if you want that terminal output included."))
        }
    }
}

/// The tooltip of an excerpt whose text is gone (`TerminalContextInlineChip`).
pub fn expired_tooltip(label: &str) -> String {
    format!("Terminal context expired. Remove and re-add {label} to include it in your message.")
}

/// `UnresolvedContextChip`'s tooltip, for a link whose context this client does not have.
pub const UNRESOLVED_TOOLTIP: &str = "This context is no longer available. Remove it or attach it again.";

#[cfg(test)]
mod tests;
