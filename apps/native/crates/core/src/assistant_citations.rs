//! Assistant quotes: `shared/assistantCitations.ts`, `lib/assistantTextSelection.ts`,
//! `chat/citationProcedure.ts` and `chat/assistantCitationCommentDismissal.ts`.

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
    if aborted {
        HistoryAction::Aborted
    } else if failed {
        HistoryAction::Failed(Failure::Load)
    } else {
        match message {
            CitedMessage::Assistant => HistoryAction::Found,
            CitedMessage::Other => HistoryAction::Failed(Failure::NotAssistant),
            CitedMessage::Missing if !more => HistoryAction::Failed(Failure::Unavailable),
            CitedMessage::Missing if pages >= 20 => HistoryAction::Failed(Failure::Load),
            _ => HistoryAction::Load,
        }
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
    let quote = normalize(quote);
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
}
