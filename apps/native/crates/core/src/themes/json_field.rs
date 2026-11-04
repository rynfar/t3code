//! JSON field token colours from `settings/ThemeImportDialog.tsx`.

use std::ops::Range;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonToken {
    Key,
    String,
    Number,
    Literal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonHighlight {
    Syntax(JsonToken),
    Selection,
}

/// Split syntax around the editor's UTF-8 selection so no foreground styles overlap.
pub fn json_highlights(tokens: &[(Range<usize>, JsonToken)], selection: Range<usize>) -> Vec<(Range<usize>, JsonHighlight)> {
    if selection.is_empty() {
        return tokens.iter().map(|(range, kind)| (range.clone(), JsonHighlight::Syntax(*kind))).collect();
    }
    let mut result = Vec::with_capacity(tokens.len() + 2);
    for (range, kind) in tokens {
        if range.start < selection.start {
            result.push((range.start..range.end.min(selection.start), JsonHighlight::Syntax(*kind)));
        }
        if range.end > selection.end {
            result.push((range.start.max(selection.end)..range.end, JsonHighlight::Syntax(*kind)));
        }
    }
    result.push((selection, JsonHighlight::Selection));
    result.sort_unstable_by_key(|(range, _)| range.start);
    result
}

/// Byte ranges follow the web's permissive regex, including partially edited JSON.
pub fn json_tokens(text: &str) -> Vec<(Range<usize>, JsonToken)> {
    if text.encode_utf16().take(20_001).count() > 20_000 {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let token = if bytes[at] == b'"' {
            let mut end = at + 1;
            while end < bytes.len() && bytes[end] != b'"' {
                if bytes[end] == b'\\' {
                    // JS `\\.` does not match a line terminator.
                    if end + 1 == bytes.len()
                        || matches!(bytes[end + 1], b'\n' | b'\r')
                        || text.get(end + 1..).is_some_and(|s| s.starts_with(['\u{2028}', '\u{2029}']))
                    {
                        break;
                    }
                    end += 1;
                    end += text[end..].chars().next().unwrap().len_utf8();
                } else {
                    end += 1;
                }
            }
            if bytes.get(end) != Some(&b'"') {
                None
            } else {
                end += 1;
                let kind = if text[end..].trim_start().starts_with(':') {
                    JsonToken::Key
                } else {
                    JsonToken::String
                };
                Some((end, kind))
            }
        } else if bytes[at].is_ascii_digit() || (bytes[at] == b'-' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit)) {
            let mut end = at + usize::from(bytes[at] == b'-');
            if bytes[end] == b'0' {
                end += 1;
            } else {
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
            }
            if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
                end += 2;
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
            }
            if matches!(bytes.get(end), Some(b'e' | b'E')) {
                let mut exponent = end + 1;
                if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
                    exponent += 1;
                }
                if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                    end = exponent + 1;
                    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                        end += 1;
                    }
                }
            }
            Some((end, JsonToken::Number))
        } else {
            ["true", "false", "null"]
                .into_iter()
                .find(|word| text[at..].starts_with(word))
                .map(|word| (at + word.len(), JsonToken::Literal))
        };
        if let Some((end, kind)) = token {
            result.push((start..end, kind));
            at = end;
        } else {
            at += text[at..].chars().next().unwrap().len_utf8();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_replaces_syntax_without_overlapping_decoration_ranges() {
        let text = r#"{"é":"Aurora","n":12,"yes":true}"#;
        let tokens = json_tokens(text);
        let word = text.find("Aurora").unwrap();
        let number = text.find("12").unwrap();
        for selection in [
            0..0,
            0..text.len(),
            word + 1..word + 5,
            2..word + 3,
            number + 1..text.len() - 1,
            word - 2..word - 1,
        ] {
            let highlights = json_highlights(&tokens, selection.clone());
            for (at, _) in text.char_indices() {
                let actual: Vec<_> = highlights.iter().filter(|(range, _)| range.contains(&at)).map(|(_, kind)| *kind).collect();
                let expected = if selection.contains(&at) {
                    Some(JsonHighlight::Selection)
                } else {
                    tokens
                        .iter()
                        .find(|(range, _)| range.contains(&at))
                        .map(|(_, kind)| JsonHighlight::Syntax(*kind))
                };
                assert_eq!(actual, expected.into_iter().collect::<Vec<_>>(), "selection {selection:?}, byte {at}");
            }
            for (range, _) in highlights {
                assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
            }
        }
        assert_eq!(json_highlights(&[], 2..5), vec![(2..5, JsonHighlight::Selection)]);
    }
    #[test]
    fn highlights_keys_escaped_strings_numbers_literals_and_unicode_byte_ranges() {
        let text = r#"{"é": "a\"b", "n": -1.25e+3, "yes": true, "no": false, "nil": null}"#;
        let tokens = json_tokens(text);
        let actual: Vec<_> = tokens.iter().map(|(range, kind)| (&text[range.clone()], *kind)).collect();
        assert_eq!(
            actual,
            vec![
                (r#""é""#, JsonToken::Key),
                (r#""a\"b""#, JsonToken::String),
                (r#""n""#, JsonToken::Key),
                ("-1.25e+3", JsonToken::Number),
                (r#""yes""#, JsonToken::Key),
                ("true", JsonToken::Literal),
                (r#""no""#, JsonToken::Key),
                ("false", JsonToken::Literal),
                (r#""nil""#, JsonToken::Key),
                ("null", JsonToken::Literal),
            ]
        );
        assert!(json_tokens(&"x".repeat(20_001)).is_empty());
        assert!(json_tokens(&"😀".repeat(10_001)).is_empty());
        assert_eq!(json_tokens("true ").len(), 1);
        assert!(json_tokens("{\"unfinished").is_empty());
        assert_eq!(json_tokens("01 1e- xtrue nullish").len(), 5);
    }
}
