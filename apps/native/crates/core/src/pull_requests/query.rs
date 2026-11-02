//! The pull request search syntax, ported from `packages/client-runtime/src/pullRequestQuery.ts`:
//! `label:`, `-label:`, `author:`, `draft:`, `review:`, `status:`/`checks:` and scoped labels
//! (`area:ui`), with everything else left as host search text.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftFilter {
    Only,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewFilter {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksFilter {
    Passing,
    Failing,
}

/// `PullRequestListFilters`: absent keys are unset, as the web spreads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<ChecksFilter>,
    /// Every group must match; any name within a group suffices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

impl ListFilters {
    pub fn is_empty(&self) -> bool {
        *self == ListFilters::default()
    }

    /// `{ ...self, ...over }`: each key `over` sets replaces this one's.
    pub fn overlay(&self, over: &ListFilters) -> ListFilters {
        ListFilters {
            draft: over.draft.or(self.draft),
            review: over.review.or(self.review),
            checks: over.checks.or(self.checks),
            labels: if over.labels.is_empty() { self.labels.clone() } else { over.labels.clone() },
            excluded_labels: if over.excluded_labels.is_empty() { self.excluded_labels.clone() } else { over.excluded_labels.clone() },
            author: over.author.clone().or_else(|| self.author.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub text: String,
    pub filters: ListFilters,
}

const MAX_QUALIFIER_VALUES: usize = 10;
const MAX_QUALIFIER_LENGTH: usize = 200;

/// The runs `(?:[^<sep>"]|"[^"]*")+` matches: separators end a run, a quoted span may hold them,
/// and a quote with no partner belongs to no run.
fn runs(raw: &str, separator: impl Fn(char) -> bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let start = index;
        let mut end = index;
        while let Some(c) = raw[end..].chars().next() {
            if separator(c) {
                break;
            }
            if c == '"' {
                match raw[end + 1..].find('"') {
                    Some(close) => end += close + 2,
                    None => break,
                }
            } else {
                end += c.len_utf8();
            }
        }
        if end > start {
            out.push(&raw[start..end]);
            index = end;
        } else {
            index += raw[index..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// JavaScript's `\s`: Unicode `White_Space` without U+0085, plus U+FEFF.
fn js_whitespace(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

/// What `.` refuses to match without the `s` flag.
fn js_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn tokens(raw: &str) -> Vec<&str> {
    runs(raw, js_whitespace)
}

/// `.replaceAll('"', "").trim()`, trimming what JavaScript's `trim` does.
fn qualifier_value(raw: &str) -> String {
    raw.replace('"', "").trim_matches(js_whitespace).to_owned()
}

fn split_qualifier_list(raw: &str) -> Vec<String> {
    runs(raw, |c| c == ',').into_iter().map(qualifier_value).filter(|part| !part.is_empty()).collect()
}

/// `value.slice(0, max)`: `max` UTF-16 code units. A pair the bound splits is dropped, since a
/// Rust string cannot hold half of one.
pub fn truncate_utf16(value: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, c) in value.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &value[..index];
        }
    }
    value
}

/// `.slice(0, max).trim()`.
fn truncate(value: &str, max: usize) -> String {
    truncate_utf16(value, max).trim_matches(js_whitespace).to_owned()
}

fn bounded_names(names: Vec<String>) -> Vec<String> {
    names.iter().take(MAX_QUALIFIER_VALUES).map(|name| truncate(name, MAX_QUALIFIER_LENGTH)).filter(|name| !name.is_empty()).collect()
}

/// `/^(-?)([A-Za-z][A-Za-z0-9_-]*):(.*)$/`.
fn qualifier(token: &str) -> Option<(bool, &str, &str)> {
    let (negated, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let colon = rest.find(':')?;
    let key = &rest[..colon];
    let mut chars = key.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    (valid && !rest[colon + 1..].contains(js_line_terminator)).then(|| (negated, key, &rest[colon + 1..]))
}

fn review_value(value: &str) -> Option<ReviewFilter> {
    match value {
        "approved" => Some(ReviewFilter::Approved),
        "changes_requested" | "changes-requested" => Some(ReviewFilter::ChangesRequested),
        "required" | "review-required" => Some(ReviewFilter::ReviewRequired),
        "none" => Some(ReviewFilter::None),
        _ => None,
    }
}

fn checks_value(value: &str) -> Option<ChecksFilter> {
    match value {
        "success" | "passing" => Some(ChecksFilter::Passing),
        "failure" | "failing" => Some(ChecksFilter::Failing),
        _ => None,
    }
}

/// `parsePullRequestQuery`.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut text: Vec<&str> = Vec::new();
    let mut labels: Vec<Vec<String>> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut filters = ListFilters::default();
    for token in tokens(raw) {
        let parsed = qualifier(token);
        let value = parsed.map(|(_, _, rest)| qualifier_value(rest)).unwrap_or_default();
        let Some((negated, key, rest)) = parsed.filter(|_| !value.is_empty()) else {
            text.push(token);
            continue;
        };
        let lower = value.to_lowercase();
        let taken = match key.to_lowercase().as_str() {
            "label" => {
                let names = bounded_names(split_qualifier_list(rest));
                let any = !names.is_empty();
                if any && negated {
                    excluded.extend(names);
                } else if any {
                    labels.push(names);
                }
                any
            }
            "author" if !negated => {
                filters.author = Some(truncate(&value, MAX_QUALIFIER_LENGTH));
                true
            }
            "draft" if !negated && (lower == "true" || lower == "false") => {
                filters.draft = Some(if lower == "true" { DraftFilter::Only } else { DraftFilter::Hide });
                true
            }
            "review" if !negated => review_value(&lower).map(|review| filters.review = Some(review)).is_some(),
            "status" | "checks" if !negated => checks_value(&lower).map(|checks| filters.checks = Some(checks)).is_some(),
            "author" | "draft" | "review" | "status" | "checks" => false,
            _ if !value.starts_with('/') => {
                let scoped = split_qualifier_list(rest).into_iter().map(|name| if name.contains(':') { name } else { format!("{key}:{name}") }).collect();
                let names = bounded_names(scoped);
                let any = !names.is_empty();
                if any && negated {
                    excluded.extend(names);
                } else if any {
                    labels.push(names);
                }
                any
            }
            _ => false,
        };
        if !taken {
            text.push(token);
        }
    }
    labels.truncate(MAX_QUALIFIER_VALUES);
    excluded.truncate(MAX_QUALIFIER_VALUES);
    filters.labels = labels;
    filters.excluded_labels = excluded;
    ParsedQuery { text: text.join(" "), filters }
}

/// `applyPullRequestDraftFilterChange`: the Draft menu shows a typed `draft:` qualifier, so
/// picking another value takes the qualifier out of the query.
pub fn apply_draft_filter_change(query: &str, menu: &ListFilters, next: &ListFilters) -> (String, ListFilters) {
    let typed = parse_query(query).filters.draft;
    if next.draft == typed.or(menu.draft) {
        return (query.to_owned(), ListFilters { draft: menu.draft, ..next.clone() });
    }
    let query = match typed {
        None => query.to_owned(),
        Some(_) => tokens(query).into_iter().filter(|token| parse_query(token).filters.draft.is_none()).collect::<Vec<_>>().join(" "),
    };
    (query, next.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(groups: &[&[&str]]) -> Vec<Vec<String>> {
        groups.iter().map(|group| group.iter().map(|name| (*name).to_owned()).collect()).collect()
    }

    #[test]
    fn splits_qualifiers_from_search_text() {
        let parsed = parse_query("fix crash label:bug,\"needs review\" -label:wip author:octocat draft:false review:changes_requested status:success");
        assert_eq!(parsed.text, "fix crash");
        assert_eq!(parsed.filters.labels, labels(&[&["bug", "needs review"]]));
        assert_eq!(parsed.filters.excluded_labels, vec!["wip".to_owned()]);
        assert_eq!(parsed.filters.author.as_deref(), Some("octocat"));
        assert_eq!(parsed.filters.draft, Some(DraftFilter::Hide));
        assert_eq!(parsed.filters.review, Some(ReviewFilter::ChangesRequested));
        assert_eq!(parsed.filters.checks, Some(ChecksFilter::Passing));
    }

    #[test]
    fn keeps_unrecognised_values_as_text() {
        let parsed = parse_query("draft:maybe -author:me review:soon checks:flaky label: url:/x");
        assert_eq!(parsed.text, "draft:maybe -author:me review:soon checks:flaky label: url:/x");
        assert!(parsed.filters.is_empty());
    }

    #[test]
    fn scopes_unknown_qualifiers_as_labels() {
        let parsed = parse_query("area:ui,team:core -priority:low");
        assert_eq!(parsed.filters.labels, labels(&[&["area:ui", "team:core"]]));
        assert_eq!(parsed.filters.excluded_labels, vec!["priority:low".to_owned()]);
        assert_eq!(parsed.text, "");
    }

    #[test]
    fn quoted_spans_keep_spaces_and_unpaired_quotes_split() {
        assert_eq!(tokens("label:\"good first issue\" x"), vec!["label:\"good first issue\"", "x"]);
        assert_eq!(tokens("a\"b c"), vec!["a", "b", "c"]);
        let parsed = parse_query("label:\"good first issue\"");
        assert_eq!(parsed.filters.labels, labels(&[&["good first issue"]]));
    }

    #[test]
    fn bounds_label_groups_and_names() {
        let query = (0..12).map(|index| format!("label:l{index}")).collect::<Vec<_>>().join(" ");
        assert_eq!(parse_query(&query).filters.labels.len(), 10);
        let many = (0..12).map(|index| format!("n{index}")).collect::<Vec<_>>().join(",");
        assert_eq!(parse_query(&format!("label:{many}")).filters.labels[0].len(), 10);
        let long = "x".repeat(250);
        assert_eq!(parse_query(&format!("author:{long}")).filters.author.map(|a| a.len()), Some(200));
    }

    #[test]
    fn separators_are_javascripts_whitespace() {
        assert_eq!(tokens("a\u{feff}b"), vec!["a", "b"]);
        assert_eq!(tokens("a\u{85}b"), vec!["a\u{85}b"]);
        assert_eq!(tokens("a\u{3000}b\u{2028}c\u{a0}d"), vec!["a", "b", "c", "d"]);
        let parsed = parse_query("label:bug\u{feff}fix");
        assert_eq!((parsed.text.as_str(), parsed.filters.labels.clone()), ("fix", labels(&[&["bug"]])));
    }

    #[test]
    fn a_quoted_line_break_keeps_the_token_as_text() {
        for line_break in ["\n", "\r", "\u{2028}", "\u{2029}"] {
            let raw = format!("label:\"a{line_break}b\"");
            let parsed = parse_query(&raw);
            assert!(parsed.filters.is_empty(), "{line_break:?}");
            assert_eq!(parsed.text, raw);
        }
        assert_eq!(parse_query("label:\"a\tb\"").filters.labels, labels(&[&["a\tb"]]));
    }

    #[test]
    fn bounds_count_utf16_units_as_slice_does() {
        let astral = "😀".repeat(150);
        assert_eq!(parse_query(&format!("author:{astral}")).filters.author.map(|a| a.encode_utf16().count()), Some(200));
        // A pair the bound splits is dropped: a Rust string cannot hold half of one.
        let split = format!("x{astral}");
        assert_eq!(parse_query(&format!("author:{split}")).filters.author.map(|a| a.encode_utf16().count()), Some(199));
        assert_eq!(parse_query(&format!("label:{split}")).filters.labels[0][0].encode_utf16().count(), 199);
        assert_eq!(truncate_utf16("ab", 5), "ab");
    }

    #[test]
    fn overlay_lets_typed_keys_win() {
        let menu = ListFilters { draft: Some(DraftFilter::Only), labels: labels(&[&["a"]]), ..Default::default() };
        let typed = ListFilters { labels: labels(&[&["b"]]), ..Default::default() };
        let merged = menu.overlay(&typed);
        assert_eq!(merged.draft, Some(DraftFilter::Only));
        assert_eq!(merged.labels, labels(&[&["b"]]));
    }

    #[test]
    fn draft_menu_change_drops_a_typed_draft_qualifier() {
        let menu = ListFilters::default();
        let next = ListFilters { draft: Some(DraftFilter::Hide), ..Default::default() };
        let (query, filters) = apply_draft_filter_change("fix draft:true", &menu, &next);
        assert_eq!(query, "fix");
        assert_eq!(filters.draft, Some(DraftFilter::Hide));
        let same = ListFilters { draft: Some(DraftFilter::Only), ..Default::default() };
        let (query, filters) = apply_draft_filter_change("fix draft:true", &menu, &same);
        assert_eq!(query, "fix draft:true");
        assert_eq!(filters.draft, None);
    }
}
