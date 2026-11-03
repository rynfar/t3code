//! Assistant quotes: `shared/assistantCitations.ts`, `lib/assistantTextSelection.ts`,
//! `chat/citationProcedure.ts`, `chat/assistantCitationCommentDismissal.ts`, and the composer
//! edits/lifetimes in `ChatComposer.tsx` and `ComposerPromptEditorTiptap.tsx`.

use serde::{Deserialize, Serialize};
use std::ops::Range;
use url::Url;

pub const MAX_TEXT: usize = 8_000;
pub const MAX_COMMENT: usize = 8_000;
const CONTEXT: usize = 32;
const PREFIX: &str = "t3-citation://v1/";
const MAX_HREF: usize = 9 * (MAX_TEXT + MAX_COMMENT) + 16_000;

pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
pub fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '\u{feff}' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub version: u8,
    pub environment_id: String,
    pub thread_id: String,
    pub message_id: String,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub prefix: String,
    pub suffix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl Citation {
    pub fn valid(&self) -> bool {
        self.version == 1
            && !self.text.trim().is_empty()
            && utf16_len(&self.text) <= MAX_TEXT
            && self.end > self.start
            && self.end <= 9_007_199_254_740_991
            && [&self.environment_id, &self.thread_id, &self.message_id].iter().all(|id| !id.is_empty() && utf16_len(id) <= 512)
            && [&self.prefix, &self.suffix].iter().all(|s| utf16_len(s) <= CONTEXT)
            && self.comment.as_ref().is_none_or(|c| utf16_len(c) <= MAX_COMMENT)
    }
    pub fn with_comment(&self, comment: &str) -> Self {
        Self { comment: (!comment.trim().is_empty()).then(|| comment.trim().to_owned()), ..self.clone() }
    }
    pub fn label(&self) -> String {
        let preview = normalize(self.comment.as_deref().filter(|c| !c.trim().is_empty()).unwrap_or(&self.text));
        if utf16_len(&preview) <= 64 { preview } else { format!("{}…", take_units(&preview, 64, false)) }
    }
    pub fn href(&self) -> String {
        let path = [&self.environment_id, &self.thread_id, &self.message_id].map(|part| encode_path(part)).join("/");
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("text", &self.text)
            .append_pair("start", &self.start.to_string())
            .append_pair("end", &self.end.to_string())
            .append_pair("prefix", &self.prefix)
            .append_pair("suffix", &self.suffix);
        if let Some(comment) = &self.comment {
            query.append_pair("comment", comment);
        }
        format!("{PREFIX}{path}?{}", query.finish())
    }
    pub fn serialize(&self) -> String {
        format!("[Assistant quote]({})", self.href())
    }

    /// A native selection is already rendered text; offsets on the wire are normalized UTF-16.
    pub fn from_selection(environment_id: String, thread_id: String, message_id: String, source: &str, range: Range<usize>) -> Option<Self> {
        let quote = source.get(range.clone())?;
        if quote.trim().is_empty() {
            return None;
        }
        let normalized = normalize(source);
        let mut start = utf16_len(&normalize(&source[..range.start]));
        if range.start > 0 && source[..range.start].chars().next_back().is_some_and(char::is_whitespace) && source[range.start..].chars().next().is_some_and(char::is_whitespace) {
            start = start.saturating_sub(1);
        }
        let end = utf16_len(&normalize(&source[..range.end]));
        let before = slice_units(&normalized, 0..start);
        let after = slice_units(&normalized, end..utf16_len(&normalized));
        Some(Self {
            version: 1,
            environment_id,
            thread_id,
            message_id,
            text: quote.to_owned(),
            start,
            end,
            prefix: take_units(&before, CONTEXT, true),
            suffix: take_units(&after, CONTEXT, false),
            comment: None,
        })
    }

    pub fn from_rendered_selection(environment: String, thread: String, message: String, source: &str, quote: &str) -> Option<Self> {
        let quote = quote.trim_end();
        let range = unique_quote_range(source, quote)?;
        let mut citation = Self::from_selection(environment, thread, message, source, range)?;
        citation.text = quote.chars().take(MAX_TEXT + 1).collect();
        Some(citation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEdit {
    pub range: Range<usize>,
    pub text: String,
}
#[cfg(test)]
impl PromptEdit {
    fn apply(&self, prompt: &str) -> String {
        format!("{}{}{}", &prompt[..self.range.start], self.text, &prompt[self.range.end..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationLink {
    pub start: usize,
    pub source: String,
    pub citation: Citation,
}
impl CitationLink {
    pub fn from_match(prompt: &str, range: Range<usize>, citation: Citation) -> Self {
        Self { start: range.start, source: prompt[range].to_owned(), citation }
    }
    pub fn replace(&self, prompt: &str, text: String) -> Option<PromptEdit> {
        let range = self.start..self.start + self.source.len();
        (prompt.get(range.clone()) == Some(self.source.as_str())).then_some(PromptEdit { range, text })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertedSpaces {
    pub before: bool,
    pub after: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentAnchor {
    pub link: CitationLink,
    pub inserted_spaces: Option<InsertedSpaces>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CommentSettlement {
    Close(Option<PromptEdit>),
    KeepOpen,
}
impl CommentAnchor {
    pub fn dismiss(&self, prompt: &str, draft: &str, reason: &str) -> CommentSettlement {
        if reason == "prompt-replaced" { return CommentSettlement::Close(None); }
        if reason == "escape-key" && let Some(spaces) = self.inserted_spaces {
            let edit = self.link.replace(prompt, String::new()).map(|mut edit| {
                if spaces.before && prompt[..edit.range.start].ends_with(' ') { edit.range.start -= 1; }
                if spaces.after && prompt[edit.range.end..].starts_with(' ') { edit.range.end += 1; }
                edit
            });
            return CommentSettlement::Close(edit);
        }
        match dismiss_comment(reason, Some(draft), self.link.citation.comment.as_deref()) {
            CommentDismissal::KeepOpen => CommentSettlement::KeepOpen,
            CommentDismissal::Close => CommentSettlement::Close(None),
            CommentDismissal::Commit(comment) => self.link.replace(prompt, self.link.citation.with_comment(&comment).serialize())
                .map_or(CommentSettlement::KeepOpen, |edit| CommentSettlement::Close(Some(edit))),
        }
    }
}

pub fn insert_citation(prompt: &str, selected: Range<usize>, citation: Citation) -> (PromptEdit, CommentAnchor) {
    let cursor = selected.end;
    let before = cursor > 0 && !prompt[..cursor].ends_with(char::is_whitespace);
    let after = !prompt[cursor..].starts_with(' ');
    let source = citation.serialize();
    let link = CitationLink { start: cursor + usize::from(before), source: source.clone(), citation };
    (PromptEdit { range: cursor..cursor + usize::from(!after), text: format!("{}{source} ", if before { " " } else { "" }) },
     CommentAnchor { link, inserted_spaces: Some(InsertedSpaces { before, after }) })
}

fn take_units(text: &str, limit: usize, tail: bool) -> String {
    let mut units = 0;
    let chars: Vec<char> = if tail { text.chars().rev().collect() } else { text.chars().collect() };
    let mut kept = Vec::new();
    for c in chars {
        if units + c.len_utf16() > limit {
            break;
        }
        units += c.len_utf16();
        kept.push(c);
    }
    if tail {
        kept.reverse();
    }
    kept.into_iter().collect()
}
fn slice_units(text: &str, range: Range<usize>) -> String {
    let mut offset = 0;
    text.chars()
        .filter(|c| {
            let start = offset;
            offset += c.len_utf16();
            start >= range.start && offset <= range.end
        })
        .collect()
}
fn encode_path(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn decode_path(text: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut iter = text.bytes();
    while let Some(b) = iter.next() {
        bytes.push(if b == b'%' { u8::from_str_radix(&String::from_utf8(vec![iter.next()?, iter.next()?]).ok()?, 16).ok()? } else { b });
    }
    String::from_utf8(bytes).ok()
}

pub fn parse(href: &str) -> Option<Citation> {
    if !href.starts_with(PREFIX) || href.len() > MAX_HREF {
        return None;
    }
    let url = Url::parse(href).ok()?;
    if url.host_str()? != "v1" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.fragment().is_some() {
        return None;
    }
    let parts: Vec<_> = url.path().strip_prefix('/')?.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    let get = |key: &str| {
        let values: Vec<_> = pairs.iter().filter(|(k, _)| k == key).collect();
        (values.len() == 1).then(|| values[0].1.to_string())
    };
    let comment = get("comment");
    if pairs.len() != 5 + usize::from(comment.is_some()) {
        return None;
    }
    let number = |key| {
        let s = get(key)?;
        (s.len() <= 16 && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse::<usize>().ok()).flatten()
    };
    let citation = Citation {
        version: 1,
        environment_id: decode_path(parts[0])?,
        thread_id: decode_path(parts[1])?,
        message_id: decode_path(parts[2])?,
        text: get("text")?,
        start: number("start")?,
        end: number("end")?,
        prefix: get("prefix")?,
        suffix: get("suffix")?,
        comment,
    };
    citation.valid().then_some(citation)
}

pub fn collect(prompt: &str) -> Vec<(Range<usize>, Citation)> {
    let mut out = Vec::new();
    for (start, rest) in prompt.match_indices("[Assistant quote](t3-citation://v1/") {
        let href_start = start + "[Assistant quote](".len();
        let Some(end) = prompt[href_start..].find(')') else {
            continue;
        };
        let end = href_start + end;
        if end - href_start <= MAX_HREF
            && let Some(citation) = parse(&prompt[href_start..end])
        {
            out.push((start..end + 1, citation));
        }
        let _ = rest;
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommentDismissal {
    Commit(String),
    Close,
    KeepOpen,
}
pub fn dismiss_comment(reason: &str, draft: Option<&str>, saved: Option<&str>) -> CommentDismissal {
    let Some(draft) = draft.filter(|_| reason != "escape-key") else {
        return CommentDismissal::Close;
    };
    if draft.trim() == saved.unwrap_or_default().trim() {
        CommentDismissal::Close
    } else if utf16_len(draft) > MAX_COMMENT {
        CommentDismissal::KeepOpen
    } else {
        CommentDismissal::Commit(draft.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitedMessage {
    Assistant,
    Other,
    Missing,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAction {
    Load,
    Found,
    Failed(Failure),
    Aborted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Load,
    Unavailable,
    NotAssistant,
}
impl Failure {
    pub fn copy(self) -> (&'static str, &'static str) {
        match self {
            Self::Load => ("Could not load the cited response", "Load earlier turns, then click the citation to try again. Your saved quote is unchanged."),
            Self::Unavailable => ("The cited response is unavailable", "It may have been removed. The selected text is still saved in your citation."),
            Self::NotAssistant => ("The citation does not refer to an assistant response", "The selected text is still saved in your citation."),
        }
    }
}
pub fn history_step(message: CitedMessage, more: bool, failed: bool, pages: usize, aborted: bool) -> HistoryAction {
    if aborted { return HistoryAction::Aborted; }
    match message {
        CitedMessage::Assistant => HistoryAction::Found,
        CitedMessage::Other => HistoryAction::Failed(Failure::NotAssistant),
        CitedMessage::Missing if failed && pages > 0 => HistoryAction::Failed(Failure::Load),
        CitedMessage::Missing if !more => HistoryAction::Failed(Failure::Unavailable),
        CitedMessage::Missing if pages >= 20 => HistoryAction::Failed(Failure::Load),
        CitedMessage::Missing => HistoryAction::Load,
    }
}

#[derive(Debug, Clone)]
pub struct CitationProcedure {
    pages: usize,
    active: bool,
}
impl Default for CitationProcedure {
    fn default() -> Self { Self { pages: 0, active: true } }
}
impl CitationProcedure {
    pub fn step(&mut self, message: CitedMessage, more: bool, failed: bool) -> HistoryAction {
        let action = history_step(message, more, failed, self.pages, !self.active);
        if action == HistoryAction::Load { self.pages += 1; } else { self.active = false; }
        action
    }
    pub fn escape(&mut self, toolbar: bool) -> bool {
        let dismissed = self.active || toolbar;
        self.active = false;
        dismissed
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CitationPhase {
    Loading,
    Expanding,
    Ready(String),
    Failed,
}
pub fn resolve_phase(failed: bool, message: CitedMessage, row: Option<&str>) -> CitationPhase {
    if failed {
        return CitationPhase::Failed;
    }
    match message {
        CitedMessage::Missing => CitationPhase::Loading,
        CitedMessage::Other => CitationPhase::Failed,
        CitedMessage::Assistant => row.map_or(CitationPhase::Expanding, |id| CitationPhase::Ready(id.to_owned())),
    }
}

/// Visible markdown text for a saved selector. Controls and destination URLs are omitted.
pub fn markdown_text(source: &str) -> String {
    fn visit(node: &markdown::mdast::Node, out: &mut String) {
        use markdown::mdast::Node;
        match node {
            Node::Text(text) => out.push_str(&text.value),
            Node::InlineCode(code) => out.push_str(&code.value),
            Node::Code(code) => out.push_str(&code.value),
            Node::InlineMath(math) => out.push_str(&math.value),
            Node::Math(math) => out.push_str(&math.value),
            Node::Image(_) | Node::Html(_) => {}
            _ => {
                if let Some(children) = node.children() {
                    for child in children {
                        visit(child, out);
                    }
                }
            }
        }
        if matches!(node, Node::Paragraph(_) | Node::Heading(_) | Node::Code(_) | Node::Blockquote(_) | Node::ListItem(_) | Node::TableCell(_) | Node::Break(_))
            && !out.ends_with('\n')
        {
            out.push('\n');
        }
    }
    let Ok(root) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
        return source.to_owned();
    };
    let mut out = String::new();
    visit(&root, &mut out);
    out.trim_end().to_owned()
}

/// Refuse ambiguous offsets rather than attach a quote to a different occurrence.
pub fn unique_quote_range(source: &str, quote: &str) -> Option<Range<usize>> {
    let normalized = normalize(source);
    let quote = normalize(quote.trim_end());
    if quote.trim().is_empty() { return None; }
    let mut matches = normalized.match_indices(&quote);
    let (start, _) = matches.next()?;
    if matches.next().is_some() { return None; }
    let end = start + quote.len();
    let mut normalized_byte = 0;
    let mut space = false;
    let mut raw_start = None;
    let mut raw_end = 0;
    for (byte, c) in source.char_indices() {
        let whitespace = c.is_whitespace() || c == '\u{feff}';
        let emitted = if whitespace { usize::from(!space) } else { c.len_utf8() };
        if emitted > 0 && normalized_byte == start { raw_start = Some(byte); }
        normalized_byte += emitted;
        if normalized_byte <= end { raw_end = byte + c.len_utf8(); }
        else { break; }
        space = whitespace;
    }
    Some(raw_start?..raw_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn citation() -> Citation {
        Citation::from_selection("env /!".into(), "thread".into(), "message".into(), "before hello 🦀 after", 7..17).unwrap()
    }
    #[test]
    fn wire_link_and_comment_match_web() {
        let c = citation().with_comment("  retry?\n");
        assert_eq!(c.href(), "t3-citation://v1/env%20%2F%21/thread/message?text=hello+%F0%9F%A6%80&start=7&end=15&prefix=before+&suffix=+after&comment=retry%3F");
        assert_eq!(parse(&c.href()), Some(c.clone()));
        assert_eq!(collect(&format!("{} next", c.serialize()))[0].1, c);
        assert_eq!(c.with_comment("  ").comment, None);
        for extra in ["&start=7", "&unknown=1", "#hash"] {
            assert!(parse(&format!("{}{extra}", c.href())).is_none());
        }
    }
    #[test]
    fn selector_offsets_are_normalized_utf16_and_context_never_splits_pairs() {
        let c = Citation::from_selection("e".into(), "t".into(), "m".into(), "🦀 a\n  b done", 9..10).unwrap();
        assert_eq!((c.start, c.end, c.text.as_str()), (5, 6, "b"));
        let c = Citation::from_selection("e".into(), "t".into(), "m".into(), &format!("🦀{}hello", "a".repeat(31)), 35..40).unwrap();
        assert_eq!(utf16_len(&c.prefix), 31);
    }
    #[test]
    fn comment_dismissal_matches_web() {
        for (reason, saved, draft) in [("outside-press", None, "needs a retry"), ("focus-out", Some("first thought"), "second thought")] {
            assert_eq!(dismiss_comment(reason, Some(draft), saved), CommentDismissal::Commit(draft.into()));
        }
        let long = "x".repeat(MAX_COMMENT + 1);
        assert_eq!(dismiss_comment("outside-press", Some(&long), None), CommentDismissal::KeepOpen);
        assert_eq!(dismiss_comment("escape-key", Some(&long), None), CommentDismissal::Close);
        assert_eq!(dismiss_comment("none", Some(" saved "), Some("saved")), CommentDismissal::Close);
        assert_eq!(dismiss_comment("none", None, None), CommentDismissal::Close);
    }
    #[test]
    fn history_procedure_matches_web() {
        use CitedMessage::*;
        use HistoryAction::*;
        assert_eq!(history_step(Assistant, false, false, 0, false), Found);
        for page in 0..20 {
            assert_eq!(history_step(Missing, true, false, page, false), Load);
        }
        assert_eq!(history_step(Missing, true, false, 20, false), Failed(Failure::Load));
        assert_eq!(history_step(Missing, false, false, 1, false), Failed(Failure::Unavailable));
        assert_eq!(history_step(Other, false, false, 0, false), Failed(Failure::NotAssistant));
        assert_eq!(history_step(Missing, true, true, 1, false), Failed(Failure::Load));
        assert_eq!(history_step(Missing, true, false, 1, true), Aborted);
        assert_eq!(resolve_phase(false, Missing, None), CitationPhase::Loading);
        assert_eq!(resolve_phase(false, Assistant, None), CitationPhase::Expanding);
        assert_eq!(resolve_phase(false, Assistant, Some("row")), CitationPhase::Ready("row".into()));
        assert_eq!(resolve_phase(false, Other, None), CitationPhase::Failed);
        assert_eq!(resolve_phase(true, Assistant, Some("row")), CitationPhase::Failed);
    }
    #[test]
    fn rendered_text_omits_markup_destinations_and_controls() {
        assert_eq!(markdown_text("# Hello **world**\n\n[A link](https://example.com) and `code`\n\n```rs\nlet x = 1;\n```"), "Hello world\nA link and code\nlet x = 1;");
        assert_eq!(unique_quote_range("same same", "same"), None);
        assert_eq!(unique_quote_range("first\nsecond", "first\n\nsecond"), Some(0..12));
    }
    #[test]
    fn cite_and_cancel_preserve_the_retained_prompt_selection() {
        for (prompt, selected) in [("keep these words", 0..10), ("α selected tail", 3..11), ("before selected tail", 7..15)] {
            let (edit, anchor) = insert_citation(prompt, selected.clone(), citation());
            let inserted = edit.apply(prompt);
            assert!(inserted.contains(&prompt[..selected.end]));
            let CommentSettlement::Close(Some(cancel)) = anchor.dismiss(&inserted, "draft", "escape-key") else { panic!("cancel must remove the new citation") };
            assert_eq!(cancel.apply(&inserted), prompt);
        }
    }
    #[test]
    fn rendered_paragraph_newlines_do_not_hide_partial_or_final_selections() {
        let source = markdown_text("An assistant says hello world");
        for (quote, expected, range) in [("world\n", "world", 24..29), ("hel\n", "hel", 18..21), ("hello world\n ", "hello world", 18..29)] {
            let c = Citation::from_rendered_selection("e".into(), "t".into(), "m".into(), &source, quote).unwrap();
            assert_eq!((c.text.as_str(), c.start..c.end), (expected, range));
            assert!(c.valid());
        }
    }
    #[test]
    fn prompt_replacement_retires_changed_or_overlength_comments() {
        let link = CitationLink { start: 0, source: citation().serialize(), citation: citation() };
        let anchor = CommentAnchor { link, inserted_spaces: None };
        for draft in ["edited comment".to_owned(), "x".repeat(MAX_COMMENT + 1)] {
            assert_eq!(anchor.dismiss("answer to the provider question", &draft, "prompt-replaced"), CommentSettlement::Close(None));
        }
    }
    #[test]
    fn loaded_citations_and_new_activations_ignore_an_old_page_error() {
        let mut loaded = CitationProcedure::default();
        assert_eq!(loaded.step(CitedMessage::Assistant, true, true), HistoryAction::Found);
        let mut retry = CitationProcedure::default();
        assert_eq!(retry.step(CitedMessage::Missing, true, true), HistoryAction::Load);
        assert_eq!(retry.step(CitedMessage::Missing, true, true), HistoryAction::Failed(Failure::Load));
    }
    #[test]
    fn noncanonical_links_keep_their_original_bytes_for_edits_and_removal() {
        let raw = "[Assistant quote](t3-citation://v1/env/thread/message?suffix=&prefix=&end=11&start=0&text=hello%20world)";
        let prompt = format!("before {raw} after {raw}");
        let (range, citation) = collect(&prompt).pop().unwrap();
        let link = CitationLink::from_match(&prompt, range.clone(), citation);
        assert_eq!(link.source, raw);
        let saved = link.replace(&prompt, link.citation.with_comment("updated").serialize()).unwrap().apply(&prompt);
        assert!(saved.starts_with(&format!("before {raw} after ")));
        assert_eq!(collect(&saved).last().unwrap().1.comment.as_deref(), Some("updated"));
        assert_eq!(link.replace(&prompt, String::new()).unwrap().apply(&prompt), format!("before {raw} after "));
        assert!(link.replace("a replaced prompt", String::new()).is_none());
    }
    #[test]
    fn escape_cancels_paging_without_a_selection_toolbar() {
        let mut procedure = CitationProcedure::default();
        assert_eq!(procedure.step(CitedMessage::Missing, true, false), HistoryAction::Load);
        assert!(procedure.escape(false));
        assert_eq!(procedure.step(CitedMessage::Assistant, false, false), HistoryAction::Aborted);
        assert_eq!(CitationProcedure::default().step(CitedMessage::Assistant, false, false), HistoryAction::Found);
    }
}
