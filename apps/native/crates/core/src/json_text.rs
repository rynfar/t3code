//! JSON as the web shows it: `JSON.stringify` (key order, number and string formatting),
//! `JSON.parse` as a detector (`isJsonText` in `apps/web/src/components/chat/V2ItemInspector.tsx`),
//! and the runs the web's JSON highlighter (`@pierre/highlights`) gives each token, named by the
//! syntax capture the theme colors them with.

use std::ops::Range;

use serde_json::{Map, Number, Value};

/// `String.prototype.trim`'s whitespace: ECMAScript WhiteSpace and LineTerminator.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// `text.trim()`.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `JSON.parse` succeeds.
pub fn parse(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// `isJsonText`: non-empty text `JSON.parse` accepts.
pub fn is_json_text(text: &str) -> bool {
    !text.is_empty() && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
}

/// `JSON.stringify(value)`.
pub fn stringify(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0);
    out
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some(2), 0);
    out
}

/// An object's entries in the order a JavaScript object holds them: array-index keys first in
/// ascending order, then the rest as inserted.
pub fn js_entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut indices: Vec<(u32, (&String, &Value))> = map.iter().filter_map(|(key, value)| array_index(key).map(|index| (index, (key, value)))).collect();
    indices.sort_by_key(|(index, _)| *index);
    indices.into_iter().map(|(_, entry)| entry).chain(map.iter().filter(|(key, _)| array_index(key).is_none())).collect()
}

/// A canonical array index: `0` to `2^32 - 2`, without leading zeros.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index < u32::MAX)
}

fn write_value(out: &mut String, value: &Value, indent: Option<usize>, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&js_number(number)),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in js_entries(map).into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_string(out, key);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(width) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', width * depth));
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `Number.prototype.toString`, which `JSON.stringify` prints numbers with.
pub fn js_number(number: &Number) -> String {
    const SAFE: u64 = 1 << 53;
    if let Some(n) = number.as_u64().filter(|n| *n <= SAFE) {
        return n.to_string();
    }
    if let Some(n) = number.as_i64().filter(|n| n.unsigned_abs() <= SAFE) {
        return n.to_string();
    }
    number.as_f64().map(js_f64).unwrap_or_else(|| number.to_string())
}

fn js_f64(value: f64) -> String {
    if value == 0. || !value.is_finite() {
        return if value.is_nan() { "NaN".into() } else if value.is_infinite() { "Infinity".into() } else { "0".into() };
    }
    // Shortest round-trip digits and the decimal exponent, as `{:e}` prints them.
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (k, n) = (digits.len() as i32, exponent + 1);
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let rest = if k > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{}{rest}e{}{}", &digits[..1], if n - 1 < 0 { '-' } else { '+' }, (n - 1).abs())
    };
    if value < 0. { format!("-{body}") } else { body }
}

/// The syntax capture the web's JSON highlighter gives a run. Whitespace takes no capture of its
/// own; it draws in the text color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonToken {
    Bracket,
    Delimiter,
    Key,
    String,
    Escape,
    Number,
    Boolean,
    Null,
}

impl JsonToken {
    /// The capture name, as the Zed-format theme keys it.
    pub fn capture(self) -> &'static str {
        match self {
            JsonToken::Bracket => "punctuation.bracket",
            JsonToken::Delimiter => "punctuation.delimiter",
            JsonToken::Key => "property.json_key",
            JsonToken::String => "string",
            JsonToken::Escape => "string.escape",
            JsonToken::Number => "number",
            JsonToken::Boolean => "boolean",
            JsonToken::Null => "constant.builtin",
        }
    }
}

/// The highlighted runs of JSON text, as byte ranges in order. Meant for text `is_json_text`
/// accepts; anything else is tokenized as far as it reads.
pub fn json_tokens(text: &str) -> Vec<(Range<usize>, JsonToken)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b'{' | b'}' | b'[' | b']' => {
                at += 1;
                tokens.push((start..at, JsonToken::Bracket));
            }
            b':' | b',' => {
                at += 1;
                tokens.push((start..at, JsonToken::Delimiter));
            }
            b'"' => {
                let end = string_end(bytes, at);
                let kind = if is_key(bytes, end) { JsonToken::Key } else { JsonToken::String };
                push_string(&mut tokens, bytes, start..end, kind);
                at = end;
            }
            b'-' | b'0'..=b'9' => {
                at += 1;
                while at < bytes.len() && matches!(bytes[at], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    at += 1;
                }
                tokens.push((start..at, JsonToken::Number));
            }
            b't' if text[at..].starts_with("true") => {
                at += 4;
                tokens.push((start..at, JsonToken::Boolean));
            }
            b'f' if text[at..].starts_with("false") => {
                at += 5;
                tokens.push((start..at, JsonToken::Boolean));
            }
            b'n' if text[at..].starts_with("null") => {
                at += 4;
                tokens.push((start..at, JsonToken::Null));
            }
            _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    tokens
}

/// Where the string opening at `start` ends, past its closing quote.
fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut at = start + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// A string is a key when a `:` follows it.
fn is_key(bytes: &[u8], end: usize) -> bool {
    bytes[end.min(bytes.len())..].iter().find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r')) == Some(&b':')
}

/// A string's runs: its text in `kind`, each escape (`\x`, `\uXXXX`) as its own run.
fn push_string(tokens: &mut Vec<(Range<usize>, JsonToken)>, bytes: &[u8], range: Range<usize>, kind: JsonToken) {
    let mut run = range.start;
    let mut at = range.start + 1;
    while at < range.end {
        if bytes[at] != b'\\' {
            at += 1;
            continue;
        }
        if run < at {
            tokens.push((run..at, kind));
        }
        let unicode = bytes.get(at + 1) == Some(&b'u') && bytes.get(at + 2..at + 6).is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));
        let end = (at + if unicode { 6 } else { 2 }).min(range.end);
        tokens.push((at..end, JsonToken::Escape));
        run = end;
        at = end;
    }
    if run < range.end {
        tokens.push((run..range.end, kind));
    }
}

#[cfg(test)]
mod tests;
