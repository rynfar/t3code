use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::composer::menu::replace_text_range;
use crate::pull_requests::wire::Actor;

/// Maximum completion items shown in a diff/PR comment popover (`PullRequestMarkdownTextarea.tsx`).
pub const MAX_COMPLETION_ITEMS: usize = 8;

/// Maximum query length forwarded to `pullRequests.filterOptions` (`PullRequestFilterOptionsInput`).
pub const MAX_COMPLETION_QUERY_CHARS: usize = 100;

/// Debounce delay in milliseconds before querying `pullRequests.filterOptions` (`useCompletions`).
pub const COMPLETION_DEBOUNCE_MS: u64 = 200;

fn token_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Mirrors `pullRequestComposerTrigger.ts`: Unicode letters/digits, combining marks cannot start.
        Regex::new(r"^(?:@([\p{L}\p{N}_][\p{L}\p{M}\p{N}_.-]*)?|([#!])([\p{L}\p{N}_-]*))$")
            .expect("valid PR completion token regex")
    })
}

/// Sigil supported by a pull request / merge request provider for `#123` / `!123` references.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum ReferenceSigil {
    #[serde(rename = "#")]
    Hash,
    #[serde(rename = "!")]
    Bang,
}

impl ReferenceSigil {
    pub const fn as_char(self) -> char {
        match self {
            Self::Hash => '#',
            Self::Bang => '!',
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hash => "#",
            Self::Bang => "!",
        }
    }

    pub fn from_char(ch: char) -> Option<Self> {
        match ch {
            '#' => Some(Self::Hash),
            '!' => Some(Self::Bang),
            _ => None,
        }
    }
}

/// Capabilities controlling which triggers activate inside PR/diff comment editors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSupport {
    pub mentions: bool,
    pub reference_sigils: Vec<ReferenceSigil>,
}

impl Default for CompletionSupport {
    fn default() -> Self {
        Self { mentions: true, reference_sigils: vec![ReferenceSigil::Hash] }
    }
}

impl CompletionSupport {
    pub fn none() -> Self {
        Self { mentions: false, reference_sigils: Vec::new() }
    }

    pub fn is_enabled(&self) -> bool {
        self.mentions || !self.reference_sigils.is_empty()
    }

    /// Extracts mention/reference support from a `PullRequestCapabilities` JSON payload.
    pub fn from_capabilities_value(value: &Value) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CapabilitiesSlice {
            #[serde(default)]
            mentions: bool,
            #[serde(default)]
            reference_sigils: Vec<ReferenceSigil>,
        }

        serde_json::from_value::<CapabilitiesSlice>(value.clone())
            .map(|slice| Self { mentions: slice.mentions, reference_sigils: slice.reference_sigils })
            .unwrap_or_else(|_| Self::none())
    }
}

/// Scope metadata passed by PR/diff hosts so comment editors can resolve mentions and references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionScope {
    pub scope_key: String,
    pub project_id: String,
    pub support: CompletionSupport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Mention,
    Reference(ReferenceSigil),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestComposerTrigger {
    pub kind: TriggerKind,
    pub sigil: char,
    pub query: String,
    pub range_start: usize,
    pub range_end: usize,
}

fn clamp_char_boundary(text: &str, byte_ix: usize) -> usize {
    let mut end = byte_ix.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((byte_ix, _)) => value[..byte_ix].to_owned(),
        None => value.to_owned(),
    }
}

/// Detects an active `@mention`, `#ref`, or `!ref` token ending at `caret` when followed by
/// whitespace or end-of-text (`detectPullRequestComposerTrigger`).
pub fn detect_pull_request_composer_trigger(
    text: &str,
    caret: usize,
    support: &CompletionSupport,
) -> Option<PullRequestComposerTrigger> {
    if !support.is_enabled() {
        return None;
    }
    let end = clamp_char_boundary(text, caret);
    if text[end..].chars().next().is_some_and(|ch| !ch.is_whitespace()) {
        return None;
    }
    let mut start = end;
    for (idx, ch) in text[..end].char_indices().rev() {
        if ch.is_whitespace() {
            break;
        }
        start = idx;
    }
    let token = &text[start..end];
    let caps = token_regex().captures(token)?;
    if let Some(ref_sigil_str) = caps.get(2).map(|m| m.as_str()) {
        let sigil = ref_sigil_str.chars().next()?;
        let ref_sigil = ReferenceSigil::from_char(sigil)?;
        if !support.reference_sigils.contains(&ref_sigil) {
            return None;
        }
        return Some(PullRequestComposerTrigger {
            kind: TriggerKind::Reference(ref_sigil),
            sigil,
            query: caps.get(3).map_or("", |m| m.as_str()).to_owned(),
            range_start: start,
            range_end: end,
        });
    }
    if !support.mentions {
        return None;
    }
    Some(PullRequestComposerTrigger {
        kind: TriggerKind::Mention,
        sigil: '@',
        query: caps.get(1).map_or("", |m| m.as_str()).to_owned(),
        range_start: start,
        range_end: end,
    })
}

/// Replaces the detected trigger token with `insert_value`, consuming a following space/tab or
/// inserting a single space (`insertPullRequestCompletion`).
pub fn insert_pull_request_completion(
    text: &str,
    trigger: &PullRequestComposerTrigger,
    insert_value: &str,
) -> (String, usize) {
    let start = clamp_char_boundary(text, trigger.range_start);
    let end = clamp_char_boundary(text, trigger.range_end).max(start);
    let followed_by_space = text[end..].starts_with([' ', '\t']);
    if !followed_by_space {
        return replace_text_range(text, start, end, &format!("{insert_value} "));
    }
    let (next, caret) = replace_text_range(text, start, end, insert_value);
    (next, (caret + 1).min(text.len() - (end - start) + insert_value.len()))
}

/// Debounce key carrying the sigil and capped query (`pullRequestCompletionKey`).
pub fn pull_request_completion_key(trigger: &PullRequestComposerTrigger) -> String {
    let query = truncate_chars(&trigger.query, MAX_COMPLETION_QUERY_CHARS);
    format!("{}{query}", trigger.sigil)
}

/// Cache key distinguishing a `(scope, sigil, query)` RPC lookup.
pub fn completion_query_cache_key(scope_key: &str, trigger: &PullRequestComposerTrigger) -> String {
    let query = truncate_chars(&trigger.query, MAX_COMPLETION_QUERY_CHARS).to_ascii_lowercase();
    format!("{scope_key}:{}:{query}", trigger.sigil)
}

/// Filter kind argument sent to `pullRequests.filterOptions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterOptionsKind {
    Authors,
    References,
}

impl FilterOptionsKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authors => "authors",
            Self::References => "references",
        }
    }
}

/// Outcome of deciding whether to fetch completion options immediately, after debounce, or skip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionSearchDecision {
    Cached,
    Immediate,
    Debounce { delay_ms: u64 },
}

/// Mirrors `useCompletions` / `debouncedPullRequestCompletionSearch`: empty queries fetch
/// immediately when uncached; typed queries debounce by `COMPLETION_DEBOUNCE_MS` unless cached.
pub fn debounced_pull_request_completion_search(
    cached: bool,
    trigger: &PullRequestComposerTrigger,
) -> CompletionSearchDecision {
    if cached {
        CompletionSearchDecision::Cached
    } else if trigger.query.is_empty() {
        CompletionSearchDecision::Immediate
    } else {
        CompletionSearchDecision::Debounce { delay_ms: COMPLETION_DEBOUNCE_MS }
    }
}

/// Builds the JSON params object for `pullRequests.filterOptions` (query capped at 100 chars).
pub fn filter_options_params(project_id: &str, trigger: &PullRequestComposerTrigger) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("projectId".to_owned(), Value::String(project_id.to_owned()));
    match trigger.kind {
        TriggerKind::Mention => {
            map.insert(
                "kind".to_owned(),
                Value::String(FilterOptionsKind::Authors.as_str().to_owned()),
            );
        }
        TriggerKind::Reference(sigil) => {
            map.insert(
                "kind".to_owned(),
                Value::String(FilterOptionsKind::References.as_str().to_owned()),
            );
            map.insert("sigil".to_owned(), Value::String(sigil.as_str().to_owned()));
        }
    }
    let query = truncate_chars(&trigger.query, MAX_COMPLETION_QUERY_CHARS);
    if !query.is_empty() {
        map.insert("query".to_owned(), Value::String(query));
    }
    Value::Object(map)
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionCandidate {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

impl From<Actor> for MentionCandidate {
    fn from(actor: Actor) -> Self {
        Self {
            login: actor.login,
            name: actor.name,
            is_bot: actor.is_bot.unwrap_or(false),
            avatar_url: actor.avatar_url,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceKind {
    Issue,
    ChangeRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceCandidate {
    pub reference: String,
    pub kind: ReferenceKind,
    pub number: u64,
    pub title: String,
    pub state: ReferenceState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionOptions {
    pub authors: Vec<MentionCandidate>,
    pub references: Vec<ReferenceCandidate>,
    pub truncated: bool,
}

impl CompletionOptions {
    /// Parses a `pullRequests.filterOptions` JSON result tolerating partial/legacy fields.
    pub fn from_filter_options_value(value: &Value) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            authors: Vec<Value>,
            #[serde(default)]
            references: Vec<Value>,
            #[serde(default)]
            truncated: bool,
        }

        let Ok(raw) = serde_json::from_value::<Raw>(value.clone()) else {
            return Self::default();
        };
        Self {
            authors: raw
                .authors
                .into_iter()
                .filter_map(|item| serde_json::from_value::<Actor>(item).ok())
                .map(MentionCandidate::from)
                .collect(),
            references: raw
                .references
                .into_iter()
                .filter_map(|item| serde_json::from_value::<ReferenceCandidate>(item).ok())
                .collect(),
            truncated: raw.truncated,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionItem {
    Mention(MentionCandidate),
    Reference { sigil: ReferenceSigil, candidate: ReferenceCandidate },
}

impl CompletionItem {
    pub fn insert_text(&self) -> String {
        match self {
            Self::Mention(author) => format!("@{}", author.login),
            Self::Reference { sigil, candidate } => {
                if candidate.reference.starts_with(sigil.as_char()) {
                    candidate.reference.clone()
                } else {
                    format!("{}{}", sigil.as_char(), candidate.number)
                }
            }
        }
    }
}

/// Narrows and ranks `@mention` candidates for `query`, capped at `limit`.
pub fn narrow_pull_request_mentions<'a>(
    authors: &'a [MentionCandidate],
    query: &str,
    limit: usize,
) -> Vec<&'a MentionCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return authors.iter().take(limit).collect();
    }
    let mut ranked: Vec<(u8, usize, &MentionCandidate)> = Vec::new();
    for (index, author) in authors.iter().enumerate() {
        let login = author.login.to_lowercase();
        let name = author.name.as_deref().unwrap_or("").to_lowercase();
        let tier = if login.starts_with(&needle) {
            Some(0)
        } else if !name.is_empty() && name.starts_with(&needle) {
            Some(1)
        } else if login.contains(&needle) {
            Some(2)
        } else if !name.is_empty() && name.contains(&needle) {
            Some(3)
        } else {
            None
        };
        if let Some(tier) = tier {
            ranked.push((tier, index, author));
        }
    }
    ranked.sort_by_key(|(tier, index, _)| (*tier, *index));
    ranked.into_iter().take(limit).map(|(_, _, author)| author).collect()
}

/// Narrows `#123` / `!123` reference candidates matching `sigil`, by number prefix or title substring.
pub fn narrow_pull_request_references<'a>(
    references: &'a [ReferenceCandidate],
    sigil: ReferenceSigil,
    query: &str,
    limit: usize,
) -> Vec<&'a ReferenceCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let sigil_char = sigil.as_char();
    let needle = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, &ReferenceCandidate)> = Vec::new();
    for (index, reference) in references.iter().enumerate() {
        if !reference.reference.starts_with(sigil_char) {
            continue;
        }
        if needle.is_empty() {
            ranked.push((0, index, reference));
            continue;
        }
        let number = reference.number.to_string();
        let title_lower = reference.title.to_lowercase();
        let tier = if number == needle {
            Some(0)
        } else if number.starts_with(&needle) {
            Some(1)
        } else if title_lower.starts_with(&needle) {
            Some(2)
        } else if title_lower.contains(&needle) {
            Some(3)
        } else {
            None
        };
        if let Some(tier) = tier {
            ranked.push((tier, index, reference));
        }
    }
    ranked.sort_by_key(|(tier, index, _)| (*tier, *index));
    ranked.into_iter().take(limit).map(|(_, _, reference)| reference).collect()
}

/// Produces the ranked completion list (max `MAX_COMPLETION_ITEMS`) for the active trigger.
pub fn pull_request_completions(
    trigger: &PullRequestComposerTrigger,
    options: &CompletionOptions,
) -> Vec<CompletionItem> {
    match trigger.kind {
        TriggerKind::Mention => narrow_pull_request_mentions(&options.authors, &trigger.query, MAX_COMPLETION_ITEMS)
            .into_iter()
            .cloned()
            .map(CompletionItem::Mention)
            .collect(),
        TriggerKind::Reference(sigil) => narrow_pull_request_references(
            &options.references,
            sigil,
            &trigger.query,
            MAX_COMPLETION_ITEMS,
        )
        .into_iter()
        .cloned()
        .map(|candidate| CompletionItem::Reference { sigil, candidate })
        .collect(),
    }
}

/// Advances the active completion index up or down with wrap-around.
pub fn nudge_completion(selected: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected.min(len - 1) as isize + delta).rem_euclid(len as isize) as usize
}

/// Tracks the active PR completion trigger, keyboard selection, and dismissal range.
#[derive(Clone, Debug, Default)]
pub struct PrCompletionState {
    pub trigger: Option<PullRequestComposerTrigger>,
    pub selected: usize,
    pub dismissed_at: Option<usize>,
}

impl PrCompletionState {
    pub fn sync_input(&mut self, text: &str, caret: usize, support: &CompletionSupport) -> bool {
        let next = detect_pull_request_composer_trigger(text, caret, support);
        if let (Some(dismissed_start), Some(candidate)) = (self.dismissed_at, next.as_ref())
            && dismissed_start == candidate.range_start
        {
            if self.trigger.is_some() {
                self.trigger = None;
                return true;
            }
            return false;
        }
        if next.is_none() || self.dismissed_at.is_some_and(|start| next.as_ref().is_some_and(|t| t.range_start != start)) {
            self.dismissed_at = None;
        }
        let changed = self.trigger != next;
        if changed {
            let query_changed = self.trigger.as_ref().map(|t| t.query.as_str())
                != next.as_ref().map(|t| t.query.as_str());
            self.trigger = next;
            if query_changed || self.trigger.is_none() {
                self.selected = 0;
            }
        }
        changed
    }

    pub fn dismiss(&mut self) -> bool {
        let Some(trigger) = self.trigger.take() else {
            return false;
        };
        self.dismissed_at = Some(trigger.range_start);
        self.selected = 0;
        true
    }

    pub fn clear_dismissed(&mut self) {
        self.dismissed_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn github_support() -> CompletionSupport {
        CompletionSupport { mentions: true, reference_sigils: vec![ReferenceSigil::Hash] }
    }

    fn gitlab_support() -> CompletionSupport {
        CompletionSupport {
            mentions: true,
            reference_sigils: vec![ReferenceSigil::Hash, ReferenceSigil::Bang],
        }
    }

    #[test]
    fn detects_mention_unicode_and_validates_token_boundaries() {
        let support = github_support();
        let start = detect_pull_request_composer_trigger("@ali", 4, &support).expect("start mention");
        assert_eq!(start.kind, TriggerKind::Mention);
        assert_eq!(start.query, "ali");
        assert_eq!((start.range_start, start.range_end), (0, 4));

        let mid = detect_pull_request_composer_trigger("cc @bob-1.2", 11, &support).expect("mid mention");
        assert_eq!(mid.query, "bob-1.2");
        assert_eq!((mid.range_start, mid.range_end), (3, 11));

        let unicode = detect_pull_request_composer_trigger("hi @josé ", 9, &support).expect("unicode name");
        assert_eq!(unicode.query, "josé");

        // Mid-word caret, leading punctuation after sigil, and unsupported sigils do not match.
        assert_eq!(detect_pull_request_composer_trigger("@alice", 3, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("@.foo", 5, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("@-bar", 5, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("#1.2", 4, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("foo@bar", 7, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("anchor#12", 9, &support), None);
        assert_eq!(detect_pull_request_composer_trigger("fix !12", 7, &support), None);
        assert_eq!(
            detect_pull_request_composer_trigger("@alice", 6, &CompletionSupport::none()),
            None
        );
    }

    #[test]
    fn detects_hash_and_bang_references_when_supported() {
        let support = gitlab_support();
        let hash = detect_pull_request_composer_trigger("see #42", 7, &support).expect("hash ref");
        assert_eq!(hash.kind, TriggerKind::Reference(ReferenceSigil::Hash));
        assert_eq!(hash.query, "42");

        let bang = detect_pull_request_composer_trigger("!98", 3, &support).expect("bang ref");
        assert_eq!(bang.kind, TriggerKind::Reference(ReferenceSigil::Bang));
        assert_eq!(bang.query, "98");
    }

    #[test]
    fn inserts_completion_respecting_spaces_tabs_and_newlines() {
        let support = github_support();
        let text = "Ask @al to review";
        let trigger = detect_pull_request_composer_trigger(text, 7, &support).unwrap();
        let (next, caret) = insert_pull_request_completion(text, &trigger, "@alice");
        assert_eq!(next, "Ask @alice to review");
        assert_eq!(caret, "Ask @alice ".len());

        // Newline after caret is NOT consumed as horizontal space; inserts a trailing space.
        let before_newline = "Ask @al\nmore";
        let trigger = detect_pull_request_composer_trigger(before_newline, 7, &support).unwrap();
        let (next, caret) = insert_pull_request_completion(before_newline, &trigger, "@alice");
        assert_eq!(next, "Ask @alice \nmore");
        assert_eq!(caret, "Ask @alice ".len());
    }

    #[test]
    fn narrows_mentions_and_filters_references_strictly_by_sigil() {
        let refs = vec![
            ReferenceCandidate {
                reference: "#120".into(),
                kind: ReferenceKind::Issue,
                number: 120,
                title: "Handle timeout".into(),
                state: ReferenceState::Open,
            },
            ReferenceCandidate {
                reference: "!12".into(),
                kind: ReferenceKind::ChangeRequest,
                number: 12,
                title: "GitLab MR".into(),
                state: ReferenceState::Open,
            },
            ReferenceCandidate {
                reference: "#12".into(),
                kind: ReferenceKind::ChangeRequest,
                number: 12,
                title: "Fix sidebar".into(),
                state: ReferenceState::Merged,
            },
        ];
        let hash_hits = narrow_pull_request_references(&refs, ReferenceSigil::Hash, "12", 8);
        assert_eq!(hash_hits.iter().map(|r| r.reference.as_str()).collect::<Vec<_>>(), vec!["#12", "#120"]);
        let bang_hits = narrow_pull_request_references(&refs, ReferenceSigil::Bang, "12", 8);
        assert_eq!(bang_hits.iter().map(|r| r.reference.as_str()).collect::<Vec<_>>(), vec!["!12"]);
    }

    #[test]
    fn caps_query_to_100_chars_and_decodes_filter_options() {
        let long_query = format!("@{}", "a".repeat(140));
        let trigger =
            detect_pull_request_composer_trigger(&long_query, long_query.len(), &github_support())
                .unwrap();
        assert_eq!(pull_request_completion_key(&trigger).len(), 1 + MAX_COMPLETION_QUERY_CHARS);
        let params = filter_options_params("proj-1", &trigger);
        assert_eq!(params["query"].as_str().unwrap().chars().count(), MAX_COMPLETION_QUERY_CHARS);

        let payload = json!({
            "authors": [{ "login": "theo", "name": "Theo", "isBot": false }],
            "references": [{
                "reference": "!77",
                "kind": "change-request",
                "number": 77,
                "title": "Add native autocomplete",
                "state": "open"
            }],
            "truncated": true
        });
        let options = CompletionOptions::from_filter_options_value(&payload);
        let bang = detect_pull_request_composer_trigger("!7", 2, &gitlab_support()).unwrap();
        let items = pull_request_completions(&bang, &options);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text(), "!77");
    }

    #[test]
    fn dismiss_hides_until_cleared_or_token_start_changes() {
        let mut state = PrCompletionState::default();
        let support = github_support();
        assert!(state.sync_input("@al", 3, &support));
        assert!(state.dismiss());
        assert!(state.trigger.is_none());
        assert!(!state.sync_input("@ali", 4, &support));
        state.clear_dismissed();
        assert!(state.sync_input("@ali", 4, &support));
        assert!(state.trigger.is_some());
    }
}
