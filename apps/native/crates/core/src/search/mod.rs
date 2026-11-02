//! Port of `packages/shared/src/paletteSearch.ts`: one ranking for the command palette and thread search.
//!
//! Offsets and ranges are char (Unicode scalar) indices into the source text, not byte offsets.
//! Not ported: `messageMatcher`, `messagePhrase`, and `MessageMatch` (message-snippet grading).

use std::cmp::{Ordering, Reverse};
use std::iter;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Both activity recency and open counts halve every two weeks.
pub const SEARCH_RECENCY_HALF_LIFE_MS: i64 = 14 * DAY_MS;
/// Opens closer together than this count once, so flipping between threads is not frequency.
pub const THREAD_OPEN_DEDUPE_MS: i64 = 10 * 60 * 1000;
pub const PALETTE_THREAD_LIMIT: usize = 20;
pub const PALETTE_MESSAGE_LIMIT: usize = 10;
/// Next to matching threads, other groups show only their best few rows.
pub const PALETTE_SECONDARY_LIMIT: usize = 3;

const THREAD_SEARCH_PICK_LIMIT: usize = 3;
const THREAD_SEARCH_PICK_MAX_LENGTH: usize = 32;
const PICK_WINDOW_MS: i64 = 21 * DAY_MS;
const PICK_PREFIX_MIN_LENGTH: usize = 3;
const USE_BOOST_LIMIT: f64 = 2.5;
const REFERENCE_PREFIX_MIN: usize = 8;
const KEYWORD_QUALITY: f64 = 0.65;
const KEYWORD_QUERY_MIN_LENGTH: usize = 4;
const MESSAGE_TOKEN_MIN_LENGTH: usize = 3;
const MESSAGE_WORD_START_LENGTH: usize = 4;
const MESSAGE_PREFIX: f64 = 0.8;

const WHOLE_WORD: f64 = 1.0;
const WHOLE_NAME: f64 = 1.5;
const MID_WORD: f64 = 0.3;
const TYPO: f64 = 0.45;
const JOINED_WORDS: f64 = 0.6;

const FILLER_WORDS: [&str; 10] = ["a", "an", "and", "for", "in", "of", "on", "the", "to", "with"];

fn is_filler(token: &str) -> bool {
    FILLER_WORDS.contains(&token)
}

/// JavaScript's `\s`, which `trim` and `split(/\s+/)` use.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `\p{L}` or `\p{N}`.
fn is_alnum(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric();
    }
    (c.is_alphabetic() && !is_combining_mark(c)) || c.is_numeric()
}

fn is_letter(c: char) -> bool {
    c.is_alphabetic() && !c.is_numeric() && !is_combining_mark(c)
}

// `\p{So}` approximated by its symbol and emoji blocks, as start and end pairs: std has no general categories.
const SYMBOL_BLOCKS: [u32; 42] = [
    0xA6, 0xA6, 0xA9, 0xA9, 0xAE, 0xAE, 0xB0, 0xB0, 0x2100, 0x214F, 0x2190, 0x244A, 0x249C, 0x24E9, 0x2500,
    0x27BF, 0x2800, 0x28FF, 0x2B00, 0x2BFF, 0x2E80, 0x2FFB, 0x3004, 0x3004, 0x3012, 0x3013, 0x3020, 0x3020,
    0x3036, 0x3037, 0x303E, 0x303F, 0x3200, 0x33FF, 0x4DC0, 0x4DFF, 0xA490, 0xA4C6, 0x1D000, 0x1D24F,
    0x1F000, 0x1FAFF,
];

fn is_symbol(c: char) -> bool {
    !c.is_ascii()
        && !is_combining_mark(c)
        && SYMBOL_BLOCKS.chunks_exact(2).any(|block| (block[0]..=block[1]).contains(&(c as u32)))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn has_cjk_word(token: &str) -> bool {
    token.chars().any(|c| is_cjk(c) || matches!(c as u32, 0xAC00..=0xD7AF))
}

fn lower_single(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// Case and accents folded; `None` when the letter folds to more or less than one char ("ß", "ﬁ").
fn fold_character(c: char) -> Option<char> {
    let mut folded = c.nfkd().filter(|d| !is_combining_mark(*d)).flat_map(char::to_lowercase);
    let first = folded.next()?;
    if folded.next().is_some() || first.len_utf16() != 1 {
        return None;
    }
    // A typed straight apostrophe finds a curly one: iOS keyboards type "’".
    Some(match first {
        'ς' => 'σ',
        '‘' | '’' => '\'',
        other => other,
    })
}

/// One char per source char, so offsets read from the source. Only BMP letters fold, as in the TS.
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    if (c as u32) <= 0xFFFF
        && let Some(folded) = fold_character(c)
    {
        return folded;
    }
    lower_single(c)
}

fn is_word_start(before_previous: Option<char>, previous: Option<char>, c: char, next: Option<char>) -> bool {
    // "Julius’s": a possessive or contraction does not start a word.
    let after_apostrophe = matches!(previous, Some('\'' | '’')) && before_previous.is_some_and(is_alnum);
    let Some(previous) = previous else { return is_alnum(c) || is_symbol(c) };
    (is_alnum(c) || is_symbol(c))
        && !after_apostrophe
        && (!is_alnum(previous)
            || (previous.is_lowercase() && c.is_uppercase())
            // The last capital of an acronym starts the next word: "JSONSchema".
            || (previous.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase))
            || previous.is_numeric() != c.is_numeric()
            || is_cjk(c))
}

#[derive(Debug, Clone)]
struct SearchField {
    text: Vec<char>,
    word_starts: Vec<bool>,
}

fn fold_search_field(source: &str) -> SearchField {
    let chars: Vec<char> = source.chars().collect();
    let at = |index: usize, back: usize| index.checked_sub(back).and_then(|i| chars.get(i)).copied();
    let word_starts = (0..chars.len())
        .map(|i| is_word_start(at(i, 2), at(i, 1), chars[i], chars.get(i + 1).copied()))
        .collect();
    SearchField { text: chars.iter().map(|&c| fold_char(c)).collect(), word_starts }
}

fn find(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= haystack.len()).then_some(from);
    }
    haystack.get(from..)?.windows(needle.len()).position(|window| window == needle).map(|at| at + from)
}

fn strip_wrapping_quotes(text: &str) -> &str {
    let is_quote = |c: char| matches!(c, '"' | '\'' | '`' | '“' | '”' | '‘' | '’');
    let leading = text.trim_start_matches(is_js_space);
    let text = if leading.starts_with(is_quote) { leading.trim_start_matches(is_quote) } else { text };
    let trailing = text.trim_end_matches(is_js_space);
    if trailing.ends_with(is_quote) { trailing.trim_end_matches(is_quote) } else { text }
}

/// Folded, whitespace-collapsed query; empty when there is nothing to match.
pub fn fold_search_query(query: &str) -> String {
    let folded: String = strip_wrapping_quotes(query).nfc().map(fold_char).collect();
    folded.split(is_js_space).filter(|token| !token.is_empty()).collect::<Vec<_>>().join(" ")
}

fn word_end(field: &SearchField, index: usize) -> usize {
    let mut end = index + 1;
    while end < field.text.len() && !field.word_starts[end] && is_alnum(field.text[end]) {
        end += 1;
    }
    end
}

fn run_end(text: &[char], from: usize) -> usize {
    let mut end = from;
    while end < text.len() && is_alnum(text[end]) {
        end += 1;
    }
    end
}

fn tail(chars: &[char], from: usize) -> &[char] {
    chars.get(from..).unwrap_or(&[])
}

/// One substitution, insertion, deletion, or swap of neighbours; with `swap_only`, only a swap.
fn within_one_edit(left: &[char], right: &[char], swap_only: bool) -> bool {
    if left.len().abs_diff(right.len()) > usize::from(!swap_only) {
        return false;
    }
    let common = left.iter().zip(right).take_while(|(l, r)| l == r).count();
    let (rest_left, rest_right) = (&left[common..], &right[common..]);
    let swapped = rest_left.len() > 1
        && rest_right.get(1) == Some(&rest_left[0])
        && rest_right.first() == Some(&rest_left[1])
        && tail(rest_left, 2) == tail(rest_right, 2);
    if swap_only {
        return swapped;
    }
    swapped
        || rest_left == rest_right
        || tail(rest_left, 1) == tail(rest_right, 1)
        || tail(rest_left, 1) == rest_right
        || rest_left == tail(rest_right, 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchRange {
    pub start: usize,
    pub end: usize,
}

struct TokenMatch {
    score: f64,
    range: SearchRange,
    /// Separate ranges, for words typed together; empty otherwise.
    letters: Vec<SearchRange>,
}

impl TokenMatch {
    fn new(score: f64, start: usize, end: usize) -> Self {
        Self { score, range: SearchRange { start, end }, letters: Vec::new() }
    }
}

/// Mid-word hits need four or more characters and `mid_word`, so "del" does not find "model".
/// Typos keep the first letter except for swaps, and numbers never match with a typo.
fn match_token(
    field: &SearchField,
    token: &[char],
    typos: bool,
    is_primary: bool,
    mid_word: bool,
) -> Option<TokenMatch> {
    let first = *token.first()?;
    let (text, len) = (&field.text, token.len());
    // A token that opens with punctuation (".ts") brings its own boundary.
    let own_boundary = !is_alnum(first) && !is_symbol(first);
    let mut best: Option<TokenMatch> = None;
    let mut next = find(text, token, 0);
    while let Some(index) = next {
        if field.word_starts[index] || own_boundary {
            let end = index + len;
            let whole = end >= text.len() || field.word_starts[end] || !is_alnum(text[end]);
            let score =
                if whole { WHOLE_WORD } else { 0.7 + 0.2 * len as f64 / (run_end(text, end) - index) as f64 };
            if best.as_ref().is_none_or(|b| score > b.score) {
                best = Some(TokenMatch::new(score, index, end));
            }
            if whole {
                return best;
            }
        } else if mid_word && len >= 4 && best.is_none() {
            best = Some(TokenMatch::new(MID_WORD, index, index + len));
        }
        next = find(text, token, index + 1);
    }
    // Joined words are exact, so a strong match elsewhere must not hide "VS Code" for "vscode".
    if is_primary
        && best.as_ref().is_none_or(|b| b.score <= MID_WORD)
        && let Some(joined) = match_joined_words(field, token)
    {
        return Some(joined);
    }
    if best.as_ref().is_some_and(|b| b.score > MID_WORD) || !typos || len < 3 {
        return best;
    }
    if token.iter().any(|&c| is_letter(c)) {
        let short = len <= 4;
        for index in (0..text.len()).filter(|&i| field.word_starts[i]) {
            // A typo keeps the first letter, unless it swaps the first two of a longer word.
            let start = text[index];
            if start != first && !(len > 3 && start == token[1] && text.get(index + 1) == Some(&first)) {
                continue;
            }
            let word = &text[index..run_end(text, index + 1)];
            // Short tokens forgive only a swap or one missing letter: "cron" is not every "con…".
            let lengths: &[usize] = if short { &[len, len + 1] } else { &[len, len - 1, len + 1] };
            for &length in lengths {
                if length <= word.len() && within_one_edit(token, &word[..length], short && length == len) {
                    return Some(TokenMatch::new(TYPO, index, index + length));
                }
            }
        }
    }
    best
}

/// Short words followed by a short prefix are noise inside a title ("inth" in "in thread").
/// Allow them at its start ("t3co"), before three letters ("gomod"), or as a whole word ("vscode").
fn match_joined_words(field: &SearchField, token: &[char]) -> Option<TokenMatch> {
    let text = &field.text;
    let first_word = field.word_starts.iter().position(|&start| start);
    let mut next = find(text, &token[..1], 0);
    while let Some(start) = next {
        next = find(text, &token[..1], start + 1);
        if !field.word_starts[start] {
            continue;
        }
        let mut runs: Vec<SearchRange> = Vec::new();
        let (mut at, mut rest) = (start, token);
        while !rest.is_empty() && at < text.len() {
            let end = word_end(field, at);
            if rest.len() <= end - at && text[at..].starts_with(rest) {
                runs.push(SearchRange { start: at, end: at + rest.len() });
                rest = &[];
            } else if rest.starts_with(&text[at..end]) {
                runs.push(SearchRange { start: at, end });
                rest = &rest[end - at..];
                at = end;
                while at < text.len() && !field.word_starts[at] {
                    at += 1;
                }
            } else {
                break;
            }
        }
        let Some(&last) = runs.last() else { continue };
        if !rest.is_empty() || runs.len() < 2 {
            continue;
        }
        if last.end == word_end(field, last.start)
            || last.end - last.start >= 3
            || Some(start) == first_word
            || runs[..runs.len() - 1].iter().all(|run| run.end - run.start >= 3)
        {
            return Some(TokenMatch {
                score: JOINED_WORDS,
                range: SearchRange { start, end: last.end },
                letters: runs,
            });
        }
    }
    None
}

#[derive(Debug, Clone, Copy, Default)]
struct FieldFlags {
    /// Only a whole-word match counts, as for a short query against keywords.
    whole_words: bool,
    /// Only a match at a word start counts, without typos: hidden keywords must not match loosely.
    keyword: bool,
    /// Where the thread lives: it helps a match but never makes it strong.
    context: bool,
    /// A token of four or more letters may match inside a word; only thread titles allow it.
    mid_word: bool,
}

const PLAIN: FieldFlags = FieldFlags { whole_words: false, keyword: false, context: false, mid_word: false };

#[derive(Debug, Clone)]
struct WeightedField {
    field: SearchField,
    weight: f64,
    flags: FieldFlags,
    /// Same text as the first field, which highlights and may hold words typed together.
    primary: bool,
}

fn weighted_fields<'a>(specs: impl IntoIterator<Item = (&'a str, f64, FieldFlags)>) -> Vec<WeightedField> {
    let mut primary: Option<&str> = None;
    let mut fields = Vec::new();
    for (index, (value, weight, flags)) in specs.into_iter().enumerate() {
        if index == 0 && !value.is_empty() {
            primary = Some(value);
        }
        if !value.is_empty() {
            fields.push(WeightedField {
                field: fold_search_field(value),
                weight,
                flags,
                primary: primary == Some(value),
            });
        }
    }
    fields
}

struct FieldsMatch {
    /// Roughly 0.3 (inside a word) to 1.5 (exact title).
    quality: f64,
    /// Every token matched at a word start, without a typo.
    strong: bool,
    primary_ranges: Vec<SearchRange>,
}

fn merge_ranges(mut ranges: Vec<SearchRange>) -> Vec<SearchRange> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<SearchRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Every query token must match some field. Quality is the mean of each token's best weighted
/// match, plus a bonus when the first field holds the whole query as a phrase or equals it.
fn match_search_fields(normalized_query: &str, fields: &[WeightedField], typos: bool) -> Option<FieldsMatch> {
    let tokens: Vec<(&str, Vec<char>)> =
        normalized_query.split(' ').filter(|t| !t.is_empty()).map(|t| (t, t.chars().collect())).collect();
    if tokens.is_empty() {
        return None;
    }
    // A phrase-style query ("fix the sidebar") need not match its filler words.
    let filler_optional = tokens.iter().any(|(token, _)| !is_filler(token));
    let mut primary_ranges = Vec::new();
    let (mut total, mut counted, mut strong) = (0.0, 0usize, true);
    for (token, chars) in &tokens {
        let (mut best, mut best_primary, mut at_word_start) = (0.0, None, false);
        for weighted in fields {
            let FieldFlags { whole_words, keyword, context, mid_word } = weighted.flags;
            if best >= weighted.weight && !weighted.primary && at_word_start {
                continue;
            }
            let Some(found) =
                match_token(&weighted.field, chars, typos && !keyword, weighted.primary, mid_word)
            else {
                continue;
            };
            let floor = if whole_words {
                WHOLE_WORD
            } else if keyword {
                0.7
            } else {
                0.0
            };
            if found.score < floor {
                continue;
            }
            if found.score >= 0.7 && !context {
                at_word_start = true;
            }
            best = f64::max(best, found.score * weighted.weight);
            if weighted.primary {
                best_primary = Some(found);
            }
        }
        if best == 0.0 {
            if filler_optional && is_filler(token) {
                continue;
            }
            return None;
        }
        strong &= at_word_start;
        if let Some(found) = best_primary {
            primary_ranges.extend(found.letters.is_empty().then_some(found.range));
            primary_ranges.extend(found.letters);
        }
        total += best;
        counted += 1;
    }
    let mut quality = total / counted as f64;
    if let Some(primary) = fields.iter().find(|field| field.primary) {
        let query: Vec<char> = normalized_query.chars().collect();
        if primary.field.text == query {
            quality += WHOLE_NAME - WHOLE_WORD;
        } else if tokens.len() > 1 && find(&primary.field.text, &query, 0).is_some() {
            quality += 0.25;
        }
    }
    Some(FieldsMatch { quality, strong, primary_ranges: merge_ranges(primary_ranges) })
}

/// Tokens of a message search. Every `long` token must match; `short` tokens only narrow hits.
/// Two Chinese, Japanese, or Korean characters count as long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSearchTokens {
    pub long: Vec<String>,
    pub short: Vec<String>,
    /// Every token, in the order typed.
    pub words: Vec<String>,
}

pub fn message_search_tokens(query: &str) -> MessageSearchTokens {
    let straight: String = strip_wrapping_quotes(query)
        .chars()
        .map(|c| if matches!(c, '‘' | '’') { '\'' } else { c })
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let words: Vec<String> =
        straight.split(is_js_space).filter(|t| !t.is_empty()).map(String::from).collect();
    let searchable = |token: &String| {
        let length = token.chars().count();
        length >= MESSAGE_TOKEN_MIN_LENGTH || (length == 2 && has_cjk_word(token))
    };
    let (long, short) = words.iter().cloned().partition(searchable);
    MessageSearchTokens { long, short, words }
}

/// Ranges of `text` holding a long message search token, ignoring case and accents. A long
/// token shorter than four characters only counts where it starts a word.
pub fn find_search_token_ranges(text: &str, query: &str) -> Vec<SearchRange> {
    let mut tokens: Vec<(Vec<char>, bool)> = Vec::new();
    for token in message_search_tokens(query).long {
        // Some tokens fold to nothing ("´´´"), which would match everywhere.
        let folded: Vec<char> = fold_search_query(&token).chars().collect();
        let needs_word_start = token.chars().count() < MESSAGE_WORD_START_LENGTH;
        match tokens.iter_mut().find(|(existing, _)| *existing == folded) {
            _ if folded.is_empty() => {}
            Some(entry) => entry.1 = needs_word_start,
            None => tokens.push((folded, needs_word_start)),
        }
    }
    if tokens.is_empty() {
        return Vec::new();
    }
    let field = fold_search_field(text);
    let mut ranges = Vec::new();
    for (token, needs_word_start) in &tokens {
        let own_boundary = !is_alnum(token[0]) && !is_symbol(token[0]);
        let mut next = find(&field.text, token, 0);
        while let Some(index) = next {
            if *needs_word_start && !own_boundary && !field.word_starts[index] {
                next = find(&field.text, token, index + 1);
                continue;
            }
            ranges.push(SearchRange { start: index, end: index + token.len() });
            next = find(&field.text, token, index + token.len());
        }
    }
    merge_ranges(ranges)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTextPart<'a> {
    pub text: &'a str,
    pub highlighted: bool,
    /// Char index of the part in the source text.
    pub start: usize,
}

/// `text` cut into plain and highlighted parts at sorted, non-overlapping char `ranges`.
pub fn split_search_text<'a>(text: &'a str, ranges: &[SearchRange]) -> Vec<SearchTextPart<'a>> {
    let byte_at = |index: usize| text.char_indices().nth(index).map_or(text.len(), |(at, _)| at);
    let slice =
        |start: usize, end: usize| text.get(byte_at(start)..byte_at(end).max(byte_at(start))).unwrap_or("");
    let part = |start: usize, end: usize, highlighted: bool| SearchTextPart {
        text: slice(start, end),
        highlighted,
        start,
    };
    let (mut parts, mut cursor) = (Vec::new(), 0);
    for range in ranges {
        if range.start > cursor {
            parts.push(part(cursor, range.start, false));
        }
        parts.push(part(range.start, range.end, true));
        cursor = range.end;
    }
    if cursor < text.chars().count() || parts.is_empty() {
        parts.push(part(cursor, usize::MAX, false));
    }
    parts
}

mod thread;
pub use thread::*;

#[cfg(test)]
mod tests;
