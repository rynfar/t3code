//! Thread scoring and ranking from `packages/shared/src/paletteSearch.ts`: opens and picks,
//! recency, reference matches, `scoreThreadSearch`, and the ranked item and group passes.

use super::*;

/// A query as a pick is stored and compared: folded, then cut to the stored UTF-16 length.
pub fn picked_query(query: &str) -> String {
    let mut units = 0;
    let cut: String = fold_search_query(query)
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= THREAD_SEARCH_PICK_MAX_LENGTH
        })
        .collect();
    cut.trim_matches(is_js_space).to_owned()
}

fn decay(age_ms: i64) -> f64 {
    (-(age_ms.max(0) as f64) / SEARCH_RECENCY_HALF_LIFE_MS as f64).exp2()
}

/// A query the thread was picked for in the palette, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadPick {
    pub query: String,
    pub at: i64,
}

/// Server-stored open history (`OrchestrationV2ThreadOpens`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOpenStats {
    pub opened_at: i64,
    pub score: f64,
    pub picks: Vec<ThreadPick>,
}

/// Next open stats after an open at `at`; `query` is the palette query that picked the thread.
pub fn record_thread_open(
    previous: Option<&ThreadOpenStats>,
    at: i64,
    query: Option<&str>,
) -> ThreadOpenStats {
    let pick = picked_query(query.unwrap_or(""));
    // Picks that no longer count are not kept or synced.
    let earlier =
        previous.into_iter().flat_map(|p| &p.picks).filter(|e| at - e.at <= PICK_WINDOW_MS).cloned();
    let picks: Vec<ThreadPick> = if pick.is_empty() {
        earlier.collect()
    } else {
        let earlier = earlier.filter(|entry| entry.query != pick).collect::<Vec<_>>();
        iter::once(ThreadPick { query: pick, at }).chain(earlier).take(THREAD_SEARCH_PICK_LIMIT).collect()
    };
    match previous {
        Some(previous) if at - previous.opened_at < THREAD_OPEN_DEDUPE_MS => {
            ThreadOpenStats { opened_at: previous.opened_at, score: previous.score, picks }
        }
        _ => {
            let score = previous.map_or(1.0, |p| p.score * decay(at - p.opened_at) + 1.0);
            ThreadOpenStats { opened_at: at, score, picks }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadPullRequest {
    pub number: u64,
    pub title: Option<String>,
    /// Values such as URLs, matched only by the whole query.
    pub references: Vec<String>,
}

/// Folded search fields of a subject, built on its first search. Build a new subject when its
/// data changes rather than mutating one that was searched.
#[derive(Debug, Clone, Default)]
pub struct PreparedFields(OnceLock<Vec<WeightedField>>);

#[derive(Debug, Clone, Default)]
pub struct ThreadSearchSubject {
    /// Matched only by the whole query, like references.
    pub id: Option<String>,
    pub title: String,
    pub branch: Option<String>,
    pub project_title: Option<String>,
    /// Only set with two or more environments; it tells same-named threads apart.
    pub environment_label: Option<String>,
    pub pull_requests: Vec<ThreadPullRequest>,
    /// Latest user activity, in epoch ms.
    pub activity_at: Option<i64>,
    pub opens: Option<ThreadOpenStats>,
    pub prepared: PreparedFields,
}

/// 1 for a stale, never-opened thread, about 2 for one active today, up to 2.5 for frequent opens.
fn thread_usage_boost(thread: &ThreadSearchSubject, now: i64) -> f64 {
    let recency = thread.activity_at.map_or(0.0, |at| decay(now.saturating_sub(at)));
    let opens = thread.opens.as_ref().map_or(0.0, |o| o.score * decay(now.saturating_sub(o.opened_at)));
    USE_BOOST_LIMIT.min(1.0 + recency + 0.5 * (1.0 + opens).log2())
}

/// IDs and URLs match only by a long prefix, or anywhere when the query holds a digit; a number
/// is never cut short (".../pull/2288" is not pull request 22889).
fn match_reference(normalized_query: &str, thread: &ThreadSearchSubject) -> bool {
    if normalized_query.encode_utf16().count() < REFERENCE_PREFIX_MIN {
        return false;
    }
    let anywhere = normalized_query.chars().any(char::is_numeric);
    let not_cut = |text: &str, end: usize| {
        !text.get(end..).and_then(|s| s.chars().next()).is_some_and(char::is_numeric)
    };
    let matches = |reference: &String| {
        let folded = reference.to_lowercase();
        if !anywhere {
            return folded.starts_with(normalized_query);
        }
        folded.find(normalized_query).is_some_and(|at| not_cut(&folded, at + normalized_query.len()))
            || (normalized_query.starts_with(folded.as_str()) && not_cut(normalized_query, folded.len()))
    };
    thread.id.as_ref().is_some_and(|id| !id.is_empty() && matches(id))
        || thread.pull_requests.iter().any(|pr| pr.references.iter().any(matches))
}

/// The latest pick of the thread for this query while it lasts: exact first, else one it starts.
fn active_pick<'t>(
    thread: &'t ThreadSearchSubject,
    normalized_query: &str,
    now: i64,
) -> Option<&'t ThreadPick> {
    let picks = thread.opens.as_ref().map(|opens| &opens.picks).filter(|picks| !picks.is_empty())?;
    let query = picked_query(normalized_query);
    let prefix = query.encode_utf16().count() >= PICK_PREFIX_MIN_LENGTH;
    let (mut exact, mut latest): (Option<&ThreadPick>, Option<&ThreadPick>) = (None, None);
    for pick in picks.iter().filter(|pick| now - pick.at <= PICK_WINDOW_MS) {
        if pick.query == query && exact.is_none_or(|e| pick.at > e.at) {
            exact = Some(pick);
        }
        let matches = pick.query == query || (prefix && pick.query.starts_with(&query));
        if matches && latest.is_none_or(|l| pick.at > l.at) {
            latest = Some(pick);
        }
    }
    exact.or(latest)
}

/// Why a thread matched when its title shows no match, for the result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatchHint {
    pub label: &'static str,
    pub text: String,
    pub ranges: Vec<SearchRange>,
}

fn searchable_branch(branch: Option<&str>) -> Option<&str> {
    // Most branches start with the app's worktree prefix, so "t3 connect" would match every thread.
    let branch = branch.filter(|branch| !branch.is_empty())?;
    let stripped = ["t3/", "t3code/"].iter().find_map(|prefix| branch.strip_prefix(prefix));
    Some(stripped.filter(|rest| rest.starts_with(|c| !is_js_space(c))).unwrap_or(branch))
}

/// The field explaining a match the title does not show: the first holding the whole query, else,
/// past the words the project or environment holds, the first holding the rest.
fn match_hint(normalized_query: &str, thread: &ThreadSearchSubject, typos: bool) -> Option<SearchMatchHint> {
    fn present(text: &Option<String>) -> Option<&str> {
        text.as_deref().filter(|text| !text.is_empty())
    }
    let mut candidates: Vec<(&'static str, String)> = thread
        .pull_requests
        .iter()
        .map(|pr| match present(&pr.title) {
            Some(title) => ("PR", format!("#{} {title}", pr.number)),
            None => ("PR", format!("#{}", pr.number)),
        })
        .collect();
    let others = [
        ("Branch", searchable_branch(thread.branch.as_deref())),
        ("Project", present(&thread.project_title)),
        ("Environment", present(&thread.environment_label)),
    ];
    candidates.extend(others.into_iter().filter_map(|(label, text)| Some((label, text?.to_owned()))));
    let holds = |text: &str, query: &str| {
        match_search_fields(query, &weighted_fields([(text, 1.0, PLAIN)]), typos).map(|m| m.primary_ranges)
    };
    let hint_for = |query: &str| {
        candidates.iter().find_map(|(label, text)| {
            holds(text, query).map(|ranges| SearchMatchHint { label, text: text.clone(), ranges })
        })
    };
    if let Some(whole) = hint_for(normalized_query) {
        return Some(whole);
    }
    let contexts: Vec<&str> =
        [present(&thread.project_title), present(&thread.environment_label)].into_iter().flatten().collect();
    let rest = normalized_query
        .split(' ')
        .filter(|token| !contexts.iter().any(|text| holds(text, token).is_some()))
        .collect::<Vec<_>>()
        .join(" ");
    if rest.is_empty() || rest == normalized_query { None } else { hint_for(&rest) }
}

fn thread_search_fields(thread: &ThreadSearchSubject) -> Vec<WeightedField> {
    let numbers: Vec<String> = thread.pull_requests.iter().map(|pr| format!("#{}", pr.number)).collect();
    let mut specs = vec![(thread.title.as_str(), 1.0, FieldFlags { mid_word: true, ..PLAIN })];
    for (pr, number) in thread.pull_requests.iter().zip(&numbers) {
        specs.push((number, 1.0, PLAIN));
        if let Some(title) = pr.title.as_deref() {
            specs.push((title, 0.5, PLAIN));
        }
    }
    if let Some(branch) = searchable_branch(thread.branch.as_deref()) {
        specs.push((branch, 0.6, PLAIN));
    }
    for context in [&thread.project_title, &thread.environment_label].into_iter().flatten() {
        specs.push((context, 0.25, FieldFlags { context: true, ..PLAIN }));
    }
    weighted_fields(specs)
}

/// Strips pull request prefixes and the worktree prefix so queries match the stored fields.
fn thread_field_query(normalized_query: &str) -> String {
    let mut query = normalized_query;
    for prefix in ["pr", "pull request", "pull"] {
        if let Some(rest) = query.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(is_js_space);
            if rest.strip_prefix('#').unwrap_or(rest).starts_with(|c: char| c.is_ascii_digit()) {
                query = rest;
                break;
            }
        }
    }
    if let Some(rest) = query.strip_prefix('#') {
        query = rest.trim_start_matches(is_js_space);
    }
    searchable_branch(Some(query)).unwrap_or(query).to_owned()
}

#[derive(Debug, Clone, PartialEq)]
enum Explain {
    None,
    Reference(String),
    Hint(SearchMatchHint),
    Deferred { field_query: String, typos: bool },
}

/// A scored result: from `score_thread_search` or `score_search_terms`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    /// Match quality times use; what results sort by.
    pub score: f64,
    pub quality: f64,
    /// The quality its group ranks by, when lower than `quality`.
    pub group_quality: Option<f64>,
    /// Every token matched at a word start.
    pub strong: bool,
    /// Matched ranges in the title.
    pub ranges: Vec<SearchRange>,
    /// When the thread was picked for this query, while that pick lasts.
    pub picked_at: Option<i64>,
    /// Picked for exactly this query, before newer picks for longer ones.
    pub picked_exactly: bool,
    explain: Explain,
}

impl SearchMatch {
    /// Why `thread` matched when its title shows no match; built on demand for the rows shown.
    pub fn explain(&self, thread: &ThreadSearchSubject) -> Option<SearchMatchHint> {
        match &self.explain {
            Explain::None => None,
            Explain::Reference(query) => {
                Some(SearchMatchHint { label: "ID or link", text: query.clone(), ranges: Vec::new() })
            }
            Explain::Hint(hint) => Some(hint.clone()),
            Explain::Deferred { field_query, typos } => match_hint(field_query, thread, *typos),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThreadSearchOptions {
    pub typos: bool,
    /// Palette picks; off where a pick would show up unexplained, such as a sidebar filter.
    pub picks: bool,
}

impl Default for ThreadSearchOptions {
    fn default() -> Self {
        Self { typos: false, picks: true }
    }
}

/// Scores a thread for a normalized query. Titles and pull request numbers count fully, pull
/// request titles and branches less, project and environment names least.
pub fn score_thread_search(
    normalized_query: &str,
    thread: &ThreadSearchSubject,
    now: i64,
    options: ThreadSearchOptions,
) -> Option<SearchMatch> {
    let fields = thread.prepared.0.get_or_init(|| thread_search_fields(thread));
    let field_query = thread_field_query(normalized_query);
    let field_match = match_search_fields(&field_query, fields, options.typos);
    let is_reference = field_match.is_none() && match_reference(normalized_query, thread);
    let found = field_match.or_else(|| {
        is_reference.then(|| FieldsMatch { quality: 0.6, strong: true, primary_ranges: Vec::new() })
    })?;
    // Several words spread over unrelated fields are a coincidence, so such a row is left out.
    let explain = if !found.primary_ranges.is_empty() {
        Explain::None
    } else if is_reference {
        Explain::Reference(normalized_query.to_owned())
    } else if field_query.contains(' ') {
        Explain::Hint(match_hint(&field_query, thread, options.typos)?)
    } else {
        Explain::Deferred { field_query, typos: options.typos }
    };
    let pick = if options.picks { active_pick(thread, normalized_query, now) } else { None };
    Some(SearchMatch {
        score: found.quality * thread_usage_boost(thread, now),
        quality: found.quality,
        // A whole title never lifts its group over a command of that name: new threads are "New thread".
        group_quality: Some(found.quality.min(WHOLE_NAME - 0.01)),
        strong: found.strong,
        ranges: found.primary_ranges,
        picked_at: pick.map(|pick| pick.at),
        picked_exactly: pick.is_some_and(|pick| pick.query == picked_query(normalized_query)),
        explain,
    })
}

/// When the user last used a thread: their last message or their last open.
pub fn thread_recent_use(thread: &ThreadSearchSubject) -> i64 {
    thread.activity_at.unwrap_or(0).max(thread.opens.as_ref().map_or(0, |opens| opens.opened_at))
}

/// A server message match, as `rank_message_matches` reads it.
pub trait MessageSearchHit {
    /// Older servers send none; their matches count as whole words.
    fn quality(&self) -> Option<f64>;
    fn hits(&self) -> Option<u64>;
    fn message_created_at(&self) -> Option<&str>;
}

#[derive(Debug, Clone, Copy)]
pub struct MessageRankOptions {
    pub picks: bool,
    /// Reserves message rows for threads the palette has not already listed by title.
    pub without_title_matches: bool,
}

impl Default for MessageRankOptions {
    fn default() -> Self {
        Self { picks: true, without_title_matches: false }
    }
}

/// Word matches first; among them a pick, then the closer match, then hits and use.
pub fn rank_message_matches<'a, 's, T: MessageSearchHit>(
    matches: &'a [T],
    subject_of: impl Fn(&T) -> Option<&'s ThreadSearchSubject>,
    normalized_query: &str,
    now: i64,
    options: MessageRankOptions,
) -> Vec<&'a T> {
    let title_options = ThreadSearchOptions { typos: false, picks: false };
    let mut ranked: Vec<(&'a T, f64, i64, f64)> = matches
        .iter()
        .filter_map(|hit| {
            let subject = subject_of(hit)?;
            if options.without_title_matches
                && score_thread_search(normalized_query, subject, now, title_options).is_some()
            {
                return None;
            }
            let picked_at = if options.picks {
                active_pick(subject, normalized_query, now).map_or(0, |pick| pick.at)
            } else {
                0
            };
            // A thread that keeps using the word outweighs a recent passing mention.
            let weight = 1.0 + (1.0 + hit.hits().unwrap_or(1) as f64).log2();
            Some((hit, hit.quality().unwrap_or(1.0), picked_at, weight * thread_usage_boost(subject, now)))
        })
        .collect();
    ranked.sort_by(|(left, lq, lp, ls), (right, rq, rp, rs)| {
        (*rq >= MESSAGE_PREFIX)
            .cmp(&(*lq >= MESSAGE_PREFIX))
            .then(rp.cmp(lp))
            .then(rq.partial_cmp(lq).unwrap_or(Ordering::Equal))
            .then(rs.partial_cmp(ls).unwrap_or(Ordering::Equal))
            .then_with(|| {
                right.message_created_at().unwrap_or("").cmp(left.message_created_at().unwrap_or(""))
            })
    });
    ranked.into_iter().map(|(hit, ..)| hit).collect()
}

// Whole words and prefixes share a class so "Compact" does not lift settings over "Compaction".
fn match_class(quality: f64) -> u8 {
    if quality >= WHOLE_NAME {
        3
    } else if quality >= 0.7 {
        2
    } else if quality >= 0.4 {
        1
    } else {
        0
    }
}

/// Use never crosses match classes; picks reorder word matches but not typos or mid-word hits.
fn compare_ranked(left: &SearchMatch, right: &SearchMatch) -> Ordering {
    let word_match = |m: &SearchMatch| match_class(m.quality).min(2);
    let exact_pick_at = |m: &SearchMatch| if m.picked_exactly { m.picked_at.unwrap_or(0) } else { 0 };
    word_match(right)
        .cmp(&word_match(left))
        .then(exact_pick_at(right).cmp(&exact_pick_at(left)))
        .then(match_class(right.quality).cmp(&match_class(left.quality)))
        .then(right.picked_at.unwrap_or(0).cmp(&left.picked_at.unwrap_or(0)))
        .then(right.score.partial_cmp(&left.score).unwrap_or(Ordering::Equal))
}

#[derive(Debug, Clone)]
pub struct RankedItem<'a, T> {
    pub item: &'a T,
    pub matched: SearchMatch,
}

#[derive(Debug, Clone)]
pub struct RankedGroup<'a, G, T> {
    pub group: &'a G,
    pub ranked: Vec<RankedItem<'a, T>>,
}

fn rank_pass<'a, T>(items: &'a [T], score: impl Fn(&T) -> Option<SearchMatch>) -> Vec<RankedItem<'a, T>> {
    items.iter().filter_map(|item| Some(RankedItem { item, matched: score(item)? })).collect()
}

/// Groups rank by match class, keeping typing stable and use from outweighing equal commands.
/// Typos are tried only when nothing matches at a word start: they add noise beside real matches.
pub fn rank_search_groups<'a, G, T>(
    groups: &'a [G],
    items_of: impl Fn(&'a G) -> &'a [T],
    score: impl Fn(&T, bool, &G) -> Option<SearchMatch>,
) -> Vec<RankedGroup<'a, G, T>> {
    let rank = |typos: bool| -> Vec<Vec<RankedItem<'a, T>>> {
        groups.iter().map(|group| rank_pass(items_of(group), |item| score(item, typos, group))).collect()
    };
    let mut ranked = rank(false);
    if !ranked.iter().flatten().any(|entry| entry.matched.strong) {
        ranked = rank(true);
    }
    let mut ranked: Vec<(u8, RankedGroup<'a, G, T>)> = groups
        .iter()
        .zip(ranked)
        .filter(|(_, items)| !items.is_empty())
        .map(|(group, mut items)| {
            items.sort_by(|left, right| compare_ranked(&left.matched, &right.matched));
            let best = items
                .iter()
                .map(|entry| match_class(entry.matched.group_quality.unwrap_or(entry.matched.quality)))
                .max()
                .unwrap_or(0);
            (best, RankedGroup { group, ranked: items })
        })
        .collect();
    ranked.sort_by_key(|(best, _)| Reverse(*best));
    ranked.into_iter().map(|(_, group)| group).collect()
}

/// `rank_search_groups` for a single list.
pub fn rank_search_items<'a, T>(
    items: &'a [T],
    score: impl Fn(&T, bool) -> Option<SearchMatch>,
) -> Vec<RankedItem<'a, T>> {
    let mut ranked = rank_pass(items, |item| score(item, false));
    if !ranked.iter().any(|entry| entry.matched.strong) {
        ranked = rank_pass(items, |item| score(item, true));
    }
    ranked.sort_by(|left, right| compare_ranked(&left.matched, &right.matched));
    ranked
}

/// Keywords count less so hidden terms cannot lift a group above a matching name. Short queries
/// need whole keywords, or "co" and "pro" would list most settings.
pub fn score_search_terms<S: AsRef<str>>(
    normalized_query: &str,
    terms: &[S],
    typos: bool,
) -> Option<SearchMatch> {
    // Filler words ("the ci") do not make a short query long.
    let meaningful =
        normalized_query.split(' ').filter(|token| !is_filler(token)).collect::<Vec<_>>().join(" ");
    let short = if meaningful.is_empty() { normalized_query } else { &meaningful }.encode_utf16().count()
        < KEYWORD_QUERY_MIN_LENGTH;
    // An alias typed in full ("pr") is deliberate, above a mere name prefix.
    let keyword = (if short { 0.8 } else { 0.6 }, FieldFlags { whole_words: short, keyword: true, ..PLAIN });
    let fields = weighted_fields(terms.iter().enumerate().map(|(index, term)| {
        let (weight, flags) = if index == 0 { (1.0, PLAIN) } else { keyword };
        (term.as_ref(), weight, flags)
    }));
    let found = match_search_fields(normalized_query, &fields, typos)?;
    Some(SearchMatch {
        score: found.quality,
        quality: found.quality,
        // A match only in hidden keywords must not put its group above a result whose name matches.
        group_quality: found.primary_ranges.is_empty().then(|| found.quality.min(KEYWORD_QUALITY)),
        strong: found.strong,
        ranges: found.primary_ranges,
        picked_at: None,
        picked_exactly: false,
        explain: Explain::None,
    })
}
