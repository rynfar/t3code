use std::ops::Range;
use std::sync::Arc;

use gpui::App;

use crate::theme::palette;

#[derive(Clone, Copy)]
pub(crate) struct FindHighlights<'a> {
    pub query: &'a str,
    pub active: Option<(&'a str, u64)>,
}

fn fold_character(character: char) -> char {
    let mut lowered = character.to_lowercase();
    let first = lowered.next().unwrap_or(character);
    match first {
        'ς' => 'σ',
        '‘' | '’' => '\'',
        _ if lowered.next().is_some() => character,
        _ => first,
    }
}

fn occurrences(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let query: String = query.chars().map(fold_character).collect();
    let mut folded = String::with_capacity(text.len());
    let mut offsets = Vec::new();
    for (offset, character) in text.char_indices() {
        offsets.push((folded.len(), offset));
        folded.push(fold_character(character));
    }
    offsets.push((folded.len(), text.len()));
    let original_offset = |index| {
        offsets
            .binary_search_by_key(&index, |&(folded, _)| folded)
            .ok()
            .map(|ix| offsets[ix].1)
    };
    folded
        .match_indices(&query)
        .filter_map(|(offset, matched)| {
            Some(original_offset(offset)?..original_offset(offset + matched.len())?)
        })
        .collect()
}

fn escape_html(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
}

pub(super) fn plain_markdown(text: &str) -> bool {
    // TextView cannot add independent paint ranges to formatted markdown.
    !text.is_empty()
        && text.trim() == text
        && !text.contains("  ")
        && text.chars().all(|ch| {
            ch.is_alphanumeric()
                || matches!(
                    ch,
                    ' ' | '.'
                        | ','
                        | '!'
                        | '?'
                        | '\''
                        | '‘'
                        | '’'
                        | '"'
                        | '“'
                        | '”'
                        | '('
                        | ')'
                        | '&'
                )
        })
        && !text
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'.' && pair[1].is_ascii_alphanumeric())
        && !text
            .split_once(". ")
            .is_some_and(|(prefix, _)| prefix.bytes().all(|ch| ch.is_ascii_digit()))
}

pub(super) fn marked_plain(
    text: &str,
    id: &str,
    find: Option<FindHighlights<'_>>,
    citation: Option<&t3_core::assistant_citations::Citation>,
    cx: &App,
) -> Option<Arc<str>> {
    if find.is_none() && citation.is_none() || !plain_markdown(text) {
        return None;
    }

    let p = palette(cx);
    let cited = citation
        .filter(|citation| citation.message_id == id)
        .and_then(|citation| t3_core::assistant_citations::find_citation_range(text, citation));
    let marks: Vec<_> = if let Some(cited) = cited {
        vec![(cited.range, p.context_chip_citation)]
    } else if let Some(find) = find {
        let active = find
            .active
            .filter(|(entry, _)| *entry == id)
            .map(|(_, ix)| ix);
        occurrences(text, find.query)
            .into_iter()
            .enumerate()
            .map(|(index, range)| {
                let color = if active == Some(index as u64) {
                    p.search_match_active_background
                } else {
                    p.search_match_background
                };
                (range, color)
            })
            .collect()
    } else {
        Vec::new()
    };
    if marks.is_empty() {
        return None;
    }
    let mut html = String::from("<p>");
    let mut previous = 0;
    for (range, color) in marks {
        escape_html(&mut html, &text[previous..range.start]);
        html.push_str(&format!(r##"<mark color="#{:06x}">"##, color >> 8));
        escape_html(&mut html, &text[range.clone()]);
        html.push_str("</mark>");
        previous = range.end;
    }
    escape_html(&mut html, &text[previous..]);
    html.push_str("</p>");
    Some(Arc::from(html))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_preserve_original_utf8_offsets_after_case_and_apostrophe_folding() {
        let text = "İ ΟΣ ος οσ don’t don't";
        assert_eq!(
            occurrences(text, "Σ")
                .iter()
                .map(|range| &text[range.clone()])
                .collect::<Vec<_>>(),
            ["Σ", "ς", "σ"]
        );
        assert_eq!(
            occurrences(text, "don't")
                .iter()
                .map(|range| &text[range.clone()])
                .collect::<Vec<_>>(),
            ["don’t", "don't"]
        );
        assert_eq!(occurrences("aaaa", "aa"), [0..2, 2..4]);
    }
}
