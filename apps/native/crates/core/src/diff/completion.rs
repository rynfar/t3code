use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::composer::menu::replace_text_range;
use crate::pull_requests::wire::Actor;

/// Maximum completion items shown in a diff/PR comment popover (`PullRequestMarkdownTextarea.tsx`).
pub const MAX_COMPLETION_ITEMS: usize = 8;

/// Debounce delay in milliseconds before querying `pullRequests.filterOptions` for a non-empty query.
pub const COMPLETION_DEBOUNCE_MS: u64 = 200;

fn trigger_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)([@#!])([A-Za-z0-9_.-]*)$").expect("valid PR completion trigger regex")
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

/// Detects an active `@mention` or `#ref` / `!ref` trigger ending at byte offset `caret`.
pub fn detect_pull_request_composer_trigger(
    text: &str,
    caret: usize,
    support: &CompletionSupport,
) -> Option<PullRequestComposerTrigger> {
    if !support.is_enabled() {
        return None;
    }
    let mut end = caret.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let caps = trigger_regex().captures(before)?;
    let sigil_match = caps.get(1)?;
    let query_match = caps.get(2)?;
    let sigil = sigil_match.as_str().chars().next()?;

    let kind = match sigil {
        '@' if support.mentions => TriggerKind::Mention,
        '#' | '!' => {
            let ref_sigil = ReferenceSigil::from_char(sigil)?;
            if !support.reference_sigils.contains(&ref_sigil) {
                return None;
            }
            TriggerKind::Reference(ref_sigil)
        }
        _ => return None,
    };

    Some(PullRequestComposerTrigger {
        kind,
        sigil,
        query: query_match.as_str().to_owned(),
        range_start: sigil_match.start(),
        range_end: end,
    })
}

/// Replaces the detected trigger token with `insert_value` followed by a trailing space and returns
/// the updated string plus the new byte caret position.
pub fn insert_pull_request_completion(
    text: &str,
    trigger: &PullRequestComposerTrigger,
    insert_value: &str,
) -> (String, usize) {
    let start = trigger.range_start.min(text.len());
    let end = trigger.range_end.min(text.len()).max(start);
    let already_spaced = text[end..].starts_with(char::is_whitespace);
    let replacement = if already_spaced { insert_value.to_owned() } else { format!("{insert_value} ") };
    let (next, caret) = replace_text_range(text, start, end, &replacement);
    if already_spaced {
        // Advance past the existing separating space so the user can keep typing naturally.
        let next_caret = text[end..]
            .chars()
            .next()
            .map(|ch| caret + ch.len_utf8())
            .unwrap_or(caret)
            .min(next.len());
        (next, next_caret)
    } else {
        (next, caret)
    }
}

/// Stable bucket key for a trigger kind + sigil.
pub fn pull_request_completion_key(trigger: &PullRequestComposerTrigger) -> &'static str {
    match trigger.kind {
        TriggerKind::Mention => "mention",
        TriggerKind::Reference(ReferenceSigil::Hash) => "reference:#",
        TriggerKind::Reference(ReferenceSigil::Bang) => "reference:!",
    }
}

/// Cache key distinguishing a `(scope, trigger_kind, sigil, query)` RPC lookup.
pub fn completion_query_cache_key(scope_key: &str, trigger: &PullRequestComposerTrigger) -> String {
    format!("{scope_key}:{}:{}", pull_request_completion_key(trigger), trigger.query.to_ascii_lowercase())
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

/// Mirrors `debouncedPullRequestCompletionSearch`: empty queries fetch immediately when uncached;
/// typed queries wait `COMPLETION_DEBOUNCE_MS` unless already cached.
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

/// Builds the JSON params object for `pullRequests.filterOptions`.
pub fn filter_options_params(project_id: &str, trigger: &PullRequestComposerTrigger) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("projectId".to_owned(), Value::String(project_id.to_owned()));
    match trigger.kind {
        TriggerKind::Mention => {
            map.insert("kind".to_owned(), Value::String(FilterOptionsKind::Authors.as_str().to_owned()));
        }
        TriggerKind::Reference(sigil) => {
            map.insert("kind".to_owned(), Value::String(FilterOptionsKind::References.as_str().to_owned()));
            map.insert("sigil".to_owned(), Value::String(sigil.as_str().to_owned()));
        }
    }
    if !trigger.query.is_empty() {
        map.insert("query".to_owned(), Value::String(trigger.query.clone()));
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
                if candidate.reference.starts_with('#') || candidate.reference.starts_with('!') {
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
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return authors.iter().take(limit).collect();
    }
    let mut ranked: Vec<(u8, usize, &MentionCandidate)> = Vec::new();
    for (index, author) in authors.iter().enumerate() {
        let login = author.login.to_ascii_lowercase();
        let name = author.name.as_deref().unwrap_or("").to_ascii_lowercase();
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

/// Narrows and ranks `#123` / `!123` reference candidates for `query`, capped at `limit`.
pub fn narrow_pull_request_references<'a>(
    references: &'a [ReferenceCandidate],
    query: &str,
    limit: usize,
) -> Vec<&'a ReferenceCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = query.trim().trim_start_matches(['#', '!']).to_ascii_lowercase();
    if needle.is_empty() {
        return references.iter().take(limit).collect();
    }
    let mut ranked: Vec<(u8, usize, &ReferenceCandidate)> = Vec::new();
    for (index, reference) in references.iter().enumerate() {
        let number = reference.number.to_string();
        let ref_lower = reference.reference.to_ascii_lowercase();
        let title_lower = reference.title.to_ascii_lowercase();
        let tier = if number == needle {
            Some(0)
        } else if number.starts_with(&needle) {
            Some(1)
        } else if number.contains(&needle) || ref_lower.contains(&needle) {
            Some(2)
        } else if title_lower.starts_with(&needle) {
            Some(3)
        } else if title_lower.contains(&needle) {
            Some(4)
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

/// Tracks the active PR completion trigger, keyboard selection, and query cache for one comment editor.
#[derive(Clone, Debug, Default)]
pub struct PrCompletionState {
    pub trigger: Option<PullRequestComposerTrigger>,
    pub selected: usize,
    pub dismissed_range: Option<(usize, usize)>,
}

impl PrCompletionState {
    pub fn sync_input(&mut self, text: &str, caret: usize, support: &CompletionSupport) -> bool {
        let next = detect_pull_request_composer_trigger(text, caret, support);
        if let (Some(dismissed), Some(candidate)) = (self.dismissed_range, next.as_ref())
            && dismissed == (candidate.range_start, candidate.range_end)
        {
            return false;
        }
        if next.is_none() {
            self.dismissed_range = None;
        }
        let changed = self.trigger != next;
        if changed {
            self.trigger = next;
            self.selected = 0;
            if self.trigger.is_some() {
                self.dismissed_range = None;
            }
        }
        changed
    }

    pub fn dismiss(&mut self) -> bool {
        let Some(trigger) = self.trigger.take() else {
            return false;
        };
        self.dismissed_range = Some((trigger.range_start, trigger.range_end));
        self.selected = 0;
        true
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
        CompletionSupport { mentions: true, reference_sigils: vec![ReferenceSigil::Hash, ReferenceSigil::Bang] }
    }

    #[test]
    fn detects_mention_at_start_and_after_whitespace() {
        let support = github_support();
        let start = detect_pull_request_composer_trigger("@ali", 4, &support).expect("start mention");
        assert_eq!(start.kind, TriggerKind::Mention);
        assert_eq!(start.query, "ali");
        assert_eq!((start.range_start, start.range_end), (0, 4));

        let mid = detect_pull_request_composer_trigger("cc @bob-1.2", 11, &support).expect("mid mention");
        assert_eq!(mid.kind, TriggerKind::Mention);
        assert_eq!(mid.query, "bob-1.2");
        assert_eq!((mid.range_start, mid.range_end), (3, 11));
    }

    #[test]
    fn ignores_mid_word_sigils_and_unsupported_capabilities() {
        let support = github_support();
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
        assert_eq!((hash.range_start, hash.range_end), (4, 7));

        let bang = detect_pull_request_composer_trigger("!98", 3, &support).expect("bang ref");
        assert_eq!(bang.kind, TriggerKind::Reference(ReferenceSigil::Bang));
        assert_eq!(bang.query, "98");
        assert_eq!((bang.range_start, bang.range_end), (0, 3));
    }

    #[test]
    fn inserts_completion_with_trailing_space_and_preserves_suffix() {
        let text = "Ask @al to review";
        let trigger = detect_pull_request_composer_trigger(text, 7, &github_support()).unwrap();
        let (next, caret) = insert_pull_request_completion(text, &trigger, "@alice");
        assert_eq!(next, "Ask @alice to review");
        assert_eq!(caret, "Ask @alice ".len());

        let tail = "See #1";
        let trigger = detect_pull_request_composer_trigger(tail, tail.len(), &github_support()).unwrap();
        let (next, caret) = insert_pull_request_completion(tail, &trigger, "#123");
        assert_eq!(next, "See #123 ");
        assert_eq!(caret, "See #123 ".len());
    }

    #[test]
    fn narrows_mentions_and_references_with_ranking_and_limit() {
        let authors: Vec<MentionCandidate> = (0..12)
            .map(|i| MentionCandidate {
                login: format!("dev{i}"),
                name: Some(format!("Developer {i}")),
                is_bot: false,
                avatar_url: None,
            })
            .collect();
        assert_eq!(narrow_pull_request_mentions(&authors, "", MAX_COMPLETION_ITEMS).len(), 8);

        let ranked_authors = vec![
            MentionCandidate { login: "x_alice".into(), name: None, is_bot: false, avatar_url: None },
            MentionCandidate { login: "alice".into(), name: None, is_bot: false, avatar_url: None },
        ];
        let hits = narrow_pull_request_mentions(&ranked_authors, "ali", 8);
        assert_eq!(hits.iter().map(|a| a.login.as_str()).collect::<Vec<_>>(), vec!["alice", "x_alice"]);

        let refs = vec![
            ReferenceCandidate {
                reference: "#120".into(),
                kind: ReferenceKind::Issue,
                number: 120,
                title: "Handle timeout".into(),
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
        let ref_hits = narrow_pull_request_references(&refs, "12", 8);
        assert_eq!(ref_hits.iter().map(|r| r.number).collect::<Vec<_>>(), vec![12, 120]);
    }

    #[test]
    fn debounces_non_empty_queries_when_uncached() {
        let support = github_support();
        let empty = detect_pull_request_composer_trigger("@", 1, &support).unwrap();
        assert_eq!(
            debounced_pull_request_completion_search(false, &empty),
            CompletionSearchDecision::Immediate
        );
        let typed = detect_pull_request_composer_trigger("@al", 3, &support).unwrap();
        assert_eq!(
            debounced_pull_request_completion_search(false, &typed),
            CompletionSearchDecision::Debounce { delay_ms: COMPLETION_DEBOUNCE_MS }
        );
        assert_eq!(
            debounced_pull_request_completion_search(true, &typed),
            CompletionSearchDecision::Cached
        );
    }

    #[test]
    fn decodes_capabilities_and_filter_options_payloads() {
        let caps = json!({ "mentions": true, "referenceSigils": ["#", "!"] });
        let support = CompletionSupport::from_capabilities_value(&caps);
        assert_eq!(support, gitlab_support());

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
        assert_eq!(options.authors.len(), 1);
        assert_eq!(options.authors[0].login, "theo");
        assert_eq!(options.references.len(), 1);
        assert_eq!(options.references[0].kind, ReferenceKind::ChangeRequest);
        assert!(options.truncated);

        let trigger = detect_pull_request_composer_trigger("!7", 2, &support).unwrap();
        let items = pull_request_completions(&trigger, &options);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text(), "!77");
    }

    #[test]
    fn dismiss_hides_same_trigger_range_until_edited_away() {
        let mut state = PrCompletionState::default();
        let support = github_support();
        assert!(state.sync_input("@al", 3, &support));
        assert!(state.trigger.is_some());
        assert!(state.dismiss());
        assert!(state.trigger.is_none());
        assert!(!state.sync_input("@al", 3, &support));
        assert!(state.sync_input("@ali", 4, &support));
        assert!(state.trigger.is_some());
    }
}
