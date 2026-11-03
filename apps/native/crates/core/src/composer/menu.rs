//! Composer menus: what `@`, `/`, `$` and `#` open, how the rows rank, and what picking one
//! writes. Ports `detectComposerTrigger`, `parseStandaloneComposerSlashCommand` and
//! `extendReplacementRangeForTrailingSpace` from `web/src/composer-logic.ts`; `replaceTextRange`,
//! `isPathWord`, `isCurrencyAmountQuery` and `serializeComposerFileLink` from
//! `shared/src/composerTrigger.ts`; `useComposerTriggerState.ts`, `composerMenuHighlight.ts`,
//! `nudgeComposerMenuHighlight` from `ChatComposer.tsx` and the item lists of
//! `useComposerMenuItems.ts`; `composerSlashCommandSearch.ts`; `web/src/providerSkillSearch.ts`;
//! the slash-menu helpers of `client-runtime/src/providerSkills.ts`; `matchComposerThreadItems`
//! from `client-runtime/src/composerThreadItems.ts`; `scoreQueryMatch` from
//! `shared/src/searchRanking.ts`; and the thread references of
//! `shared/src/composerContextReferences.ts`.
//!
//! Offsets are byte offsets into the prompt, as the textarea counts them; the web counts UTF-16
//! units. Search positions count chars.

use serde_json::{Value, json};

use crate::palette::{PaletteThread, thread_search_subject};
use crate::search::{ThreadSearchOptions, fold_search_query, rank_search_items, score_thread_search};
use crate::server_config::{ProviderSkill, ProviderSlashCommand, ServerProvider, WorkspaceSnapshot};

/// `COMPOSER_PATH_SEARCH_LIMIT` and `PROJECT_PATH_SEARCH_DEBOUNCE_MS` (`web/src/state/queries.ts`).
pub const PATH_SEARCH_LIMIT: usize = 80;
pub const PATH_SEARCH_DEBOUNCE_MS: u64 = 120;
const THREAD_RESULT_LIMIT: usize = 5;
/// `PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS`.
pub const WORKSPACE_SNAPSHOT_TTL_MS: i64 = 5 * 60_000;

pub const SEARCH_ENTRIES: &str = "projects.searchEntries";
pub const REFRESH_PROVIDERS: &str = "server.refreshProviders";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Path,
    PullRequest,
    SlashCommand,
    Skill,
}

/// `ComposerTrigger`: the token under the caret that opened a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub query: String,
    pub range_start: usize,
    pub range_end: usize,
}

fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\t' | '\r')
}

fn clamp_cursor(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn token_start(text: &str, cursor: usize) -> usize {
    text[..cursor].char_indices().rev().find(|(_, c)| is_whitespace(*c)).map_or(0, |(index, c)| index + c.len_utf8())
}

/// `isPathWord`: a second slash makes the whole word a path, even when the caret precedes it.
pub fn is_path_word(text: &str, word_start: usize, cursor: usize) -> bool {
    let rest = &text[cursor..];
    let word_end = cursor + rest.find(is_whitespace).unwrap_or(rest.len());
    text.get(word_start + 1..word_end).is_some_and(|word| word.contains('/'))
}

/// `isCurrencyAmountQuery`: `$5`, `€9.99` and `$10k.` are money, not skills.
pub fn is_currency_amount_query(query: &str) -> bool {
    let bytes = query.as_bytes();
    let at = |index: usize| bytes.get(index).copied();
    let mut index = usize::from(at(0) == Some(b'.'));
    if !at(index).is_some_and(|byte| byte.is_ascii_digit()) {
        return false;
    }
    index += 1;
    while at(index).is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b',' | b'.' | b'_')) {
        index += 1;
    }
    match at(index) {
        Some(b'k' | b'K' | b'm' | b'M' | b'b' | b'B' | b't' | b'T') => index += 1,
        Some(b'e' | b'E') if at(index + 1).is_some_and(|byte| byte.is_ascii_digit()) => {
            index += 1;
            while at(index).is_some_and(|byte| byte.is_ascii_digit()) {
                index += 1;
            }
        }
        _ => {}
    }
    bytes[index..].iter().all(|byte| matches!(byte, b'.' | b',' | b';' | b':' | b'!' | b'?' | b')'))
}

/// Unicode's currency symbols (`\p{Sc}`).
fn is_currency_symbol(c: char) -> bool {
    matches!(c as u32,
        0x24 | 0xA2..=0xA5 | 0x58F | 0x60B | 0x7FE..=0x7FF | 0x9F2..=0x9F3 | 0x9FB | 0xAF1 | 0xBF9 | 0xE3F
        | 0x17DB | 0x20A0..=0x20C0 | 0xA838 | 0xFDFC | 0xFE69 | 0xFF04 | 0xFFE0..=0xFFE1 | 0xFFE5..=0xFFE6
        | 0x11FDD..=0x11FE0 | 0x1E2FF | 0x1ECB0)
}

/// `^#([\p{L}\p{N}][\p{L}\p{N}_-]*)?$` after the `#`.
fn is_pull_request_query(query: &str) -> bool {
    let mut chars = query.chars();
    match chars.next() {
        None => true,
        Some(first) => first.is_alphanumeric() && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-'),
    }
}

/// `detectComposerTrigger`.
pub fn detect_trigger(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = clamp_cursor(text, cursor);
    let start = token_start(text, cursor);
    let token = &text[start..cursor];
    let trigger = |kind, query: &str| Some(Trigger { kind, query: query.to_owned(), range_start: start, range_end: cursor });
    if let Some(query) = token.strip_prefix('/') {
        if is_path_word(text, start, cursor) {
            return None;
        }
        return trigger(TriggerKind::SlashCommand, query);
    }
    if let Some(query) = token.strip_prefix('#')
        && is_pull_request_query(query)
    {
        return trigger(TriggerKind::PullRequest, query);
    }
    if let Some(symbol) = token.chars().next().filter(|c| is_currency_symbol(*c)) {
        let query = &token[symbol.len_utf8()..];
        if is_currency_amount_query(query) {
            return None;
        }
        return trigger(TriggerKind::Skill, query);
    }
    token.strip_prefix('@').and_then(|query| trigger(TriggerKind::Path, query))
}

/// `useComposerTriggerState`: Escape dismisses the trigger it closed until the caret leaves that
/// token, so typing on does not reopen the same menu.
#[derive(Debug, Clone, Default)]
pub struct TriggerState {
    trigger: Option<Trigger>,
    dismissed: Option<Trigger>,
}

impl TriggerState {
    pub fn trigger(&self) -> Option<&Trigger> {
        self.trigger.as_ref()
    }

    pub fn resolve(&self, candidate: Option<Trigger>) -> Option<Trigger> {
        match (&candidate, &self.dismissed) {
            (Some(candidate), Some(dismissed))
                if candidate.kind == dismissed.kind && candidate.range_start == dismissed.range_start =>
            {
                None
            }
            _ => candidate,
        }
    }

    /// Returns whether the active trigger changed.
    pub fn set(&mut self, candidate: Option<Trigger>) -> bool {
        let clear_dismissal = candidate.is_none();
        let active = self.resolve(candidate);
        if clear_dismissal || active.is_some() {
            self.dismissed = None;
        }
        std::mem::replace(&mut self.trigger, active) != self.trigger
    }

    pub fn dismiss(&mut self, candidate: Option<Trigger>) {
        self.dismissed = candidate;
        self.trigger = None;
    }

    pub fn reset(&mut self, candidate: Option<Trigger>) {
        self.dismissed = None;
        self.trigger = candidate;
    }
}

/// `parseSideChatCommand`: `/side [prompt]`, or its alias `/btw [prompt]`, as the whole message.
pub fn parse_side_chat_command(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, prompt) = rest.split_at(rest.find(char::is_whitespace).unwrap_or(rest.len()));
    (name.eq_ignore_ascii_case("side") || name.eq_ignore_ascii_case("btw")).then(|| prompt.trim().to_owned())
}

/// `parseStandaloneComposerSlashCommand`: `/plan` or `/default` as the whole message switches
/// the mode instead of sending.
pub fn parse_standalone_slash_command(text: &str) -> Option<SlashCommand> {
    let command = text.trim().strip_prefix('/')?.trim_end();
    match command.to_lowercase().as_str() {
        "plan" => Some(SlashCommand::Plan),
        "default" => Some(SlashCommand::Default),
        _ => None,
    }
}

/// `replaceTextRange`: the new prompt and the caret after the replacement.
pub fn replace_text_range(text: &str, range_start: usize, range_end: usize, replacement: &str) -> (String, usize) {
    let start = clamp_cursor(text, range_start);
    let end = clamp_cursor(text, range_end).max(start);
    (format!("{}{replacement}{}", &text[..start], &text[end..]), start + replacement.len())
}

/// `extendReplacementRangeForTrailingSpace`: a replacement ending in a space swallows one space
/// already after the range.
pub fn extend_range_for_trailing_space(text: &str, range_end: usize, replacement: &str) -> usize {
    if replacement.ends_with(' ') && text.as_bytes().get(range_end) == Some(&b' ') { range_end + 1 } else { range_end }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `serializeComposerFileLink`: how a file mention reads in the message, `[name](path)`.
pub fn serialize_file_link(path: &str) -> String {
    crate::files::composer_file_link(path)
}

/// What a file mention inserts: the link and a space.
pub fn file_mention_text(path: &str) -> String {
    format!("{} ", serialize_file_link(path))
}

// --- Search ranking (`searchRanking.ts`) ---

fn char_len(text: &str) -> i64 {
    text.chars().count() as i64
}

fn char_index(text: &str, byte: usize) -> i64 {
    char_len(&text[..byte])
}

/// `normalizeSearchQuery`: trimmed, leading `strip` characters removed, lowercased.
pub fn normalize_search_query(input: &str, strip: impl Fn(char) -> bool) -> String {
    input.trim().trim_start_matches(strip).to_lowercase()
}

/// `scoreSubsequenceMatch` without `ignorePosition`.
fn score_subsequence(value: &str, query: &str) -> Option<i64> {
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let (mut matched, mut first, mut previous, mut gaps) = (0usize, None::<i64>, None::<i64>, 0i64);
    for (index, c) in value.chars().enumerate() {
        let index = index as i64;
        if c != query[matched] {
            continue;
        }
        let first = *first.get_or_insert(index);
        if let Some(previous) = previous {
            gaps += index - previous - 1;
        }
        previous = Some(index);
        matched += 1;
        if matched == query.len() {
            let span = index - first + 1 - query.len() as i64;
            let length = (char_len(value) - query.len() as i64).min(64);
            return Some(first * 2 + gaps * 3 + span + length);
        }
    }
    None
}

/// The bases `scoreQueryMatch` adds for each kind of match.
#[derive(Debug, Clone, Copy)]
struct Bases {
    exact: i64,
    prefix: Option<i64>,
    boundary: Option<i64>,
    includes: Option<i64>,
    fuzzy: Option<i64>,
    markers: &'static [&'static str],
}

const DEFAULT_MARKERS: &[&str] = &[" ", "-", "_", "/"];
const WORD_MARKERS: &[&str] = &["-", "_", "/"];

impl Bases {
    const fn new(exact: i64, prefix: i64, boundary: Option<i64>, includes: i64, fuzzy: Option<i64>, markers: &'static [&'static str]) -> Bases {
        Bases { exact, prefix: Some(prefix), boundary, includes: Some(includes), fuzzy, markers }
    }
}

/// `scoreQueryMatch`; `value` and `query` are already trimmed and lowercased.
fn score_query_match(value: &str, query: &str, bases: Bases) -> Option<i64> {
    if value.is_empty() || query.is_empty() {
        return None;
    }
    let penalty = (char_len(value) - char_len(query)).clamp(0, 64);
    if value == query {
        return Some(bases.exact);
    }
    if let Some(prefix) = bases.prefix
        && value.starts_with(query)
    {
        return Some(prefix + penalty);
    }
    if let Some(boundary) = bases.boundary {
        let best = bases
            .markers
            .iter()
            .filter_map(|marker| value.find(&format!("{marker}{query}")).map(|index| char_index(value, index) + char_len(marker)))
            .min();
        if let Some(index) = best {
            return Some(boundary + index * 2 + penalty);
        }
    }
    if let Some(includes) = bases.includes
        && let Some(index) = value.find(query)
    {
        return Some(includes + char_index(value, index) * 2 + penalty);
    }
    bases.fuzzy.and_then(|fuzzy| score_subsequence(value, query).map(|score| fuzzy + score))
}

fn best_score(scores: impl IntoIterator<Item = Option<i64>>) -> Option<i64> {
    scores.into_iter().flatten().min()
}

/// Sorts by score, then tie-breaker, keeping the input order for equal pairs.
fn rank<T>(mut scored: Vec<(T, i64, String)>) -> Vec<T> {
    scored.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));
    scored.into_iter().map(|(item, _, _)| item).collect()
}

// --- Provider skills and commands (`providerSkills.ts`, `providerSkillSearch.ts`) ---

/// `formatProviderSkillDisplayName`.
pub fn skill_display_name(skill: &ProviderSkill) -> &str {
    skill.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(&skill.name)
}

/// `isProviderSkillUserInvocable`: disabled and agent-only skills stay out of the menus.
pub fn is_user_invocable(skill: &ProviderSkill) -> bool {
    skill.enabled && skill.user_invocable != Some(false)
}

fn dedupe_by_name(skills: impl IntoIterator<Item = ProviderSkill>) -> Vec<ProviderSkill> {
    let mut seen = std::collections::HashSet::new();
    skills.into_iter().filter(|skill| seen.insert(skill.name.trim().to_lowercase())).collect()
}

/// `ProviderSkillSourceKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    App,
    Repo,
    Project,
    Personal,
    System,
    Other,
}

impl SkillSource {
    pub fn label(self) -> &'static str {
        match self {
            SkillSource::App => "App",
            SkillSource::Repo => "Repo",
            SkillSource::Project => "Project",
            SkillSource::Personal => "Personal",
            SkillSource::System => "System",
            SkillSource::Other => "Provider",
        }
    }
}

/// `resolveProviderSkillSourceKind`.
pub fn skill_source(skill: &ProviderSkill) -> SkillSource {
    let path = skill.path.replace('\\', "/");
    if path.contains("/.codex/plugins/") || path.contains("/.agents/plugins/") {
        return SkillSource::App;
    }
    match skill.scope.as_deref().map(|scope| scope.trim().to_lowercase()).as_deref() {
        Some("repo" | "repository") => SkillSource::Repo,
        Some("project" | "workspace" | "local") => SkillSource::Project,
        Some("user" | "personal") => SkillSource::Personal,
        Some("system") => SkillSource::System,
        _ => SkillSource::Other,
    }
}

/// The snapshot a provider reports for `cwd`, from the config or one this client fetched.
pub fn workspace_snapshot<'a>(
    provider: &'a ServerProvider,
    fetched: Option<&'a WorkspaceSnapshot>,
    cwd: Option<&str>,
) -> Option<&'a WorkspaceSnapshot> {
    let cwd = cwd?;
    provider.workspace_snapshots.iter().find(|snapshot| snapshot.cwd == cwd).or(fetched.filter(|snapshot| snapshot.cwd == cwd))
}

/// `resolveProviderSkillsForCwd` and `resolveProviderSlashCommandsForCwd`.
pub fn skills_and_commands(provider: &ServerProvider, snapshot: Option<&WorkspaceSnapshot>) -> (Vec<ProviderSkill>, Vec<ProviderSlashCommand>) {
    match snapshot {
        Some(snapshot) => (snapshot.skills.clone(), snapshot.slash_commands.clone()),
        None => (provider.skills.clone(), provider.slash_commands.clone()),
    }
}

/// Whether a snapshot for `cwd` is complete and inside the TTL, so no scan is due.
pub fn snapshot_is_current(snapshot: Option<&WorkspaceSnapshot>, now_ms: i64) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot.slash_commands_pending != Some(true)
            && crate::sidebar::time::parse_iso(&snapshot.checked_at).is_some_and(|at| now_ms - at < WORKSPACE_SNAPSHOT_TTL_MS)
    })
}

/// `server.refreshProviders` for one provider's view of `cwd`.
pub fn refresh_providers_request(instance_id: &str, cwd: &str) -> Value {
    crate::providers::RefreshProviders::Workspace { instance_id, cwd }.payload()
}

/// The snapshot for `cwd` in a `server.refreshProviders` reply.
pub fn refreshed_snapshot(reply: &Value, instance_id: &str, cwd: &str) -> Option<WorkspaceSnapshot> {
    reply
        .get("providers")?
        .as_array()?
        .iter()
        .find(|provider| provider.get("instanceId").and_then(Value::as_str) == Some(instance_id))?
        .get("workspaceSnapshots")?
        .as_array()?
        .iter()
        .find(|snapshot| snapshot.get("cwd").and_then(Value::as_str) == Some(cwd))
        .and_then(|snapshot| serde_json::from_value(snapshot.clone()).ok())
}

/// `scoreProviderSkill`.
fn score_skill(skill: &ProviderSkill, query: &str) -> Option<i64> {
    let lower = |text: Option<&str>| text.unwrap_or_default().to_lowercase();
    best_score([
        score_query_match(&skill.name.to_lowercase(), query, Bases::new(0, 2, Some(4), 6, Some(100), WORD_MARKERS)),
        score_query_match(&skill_display_name(skill).to_lowercase(), query, Bases::new(1, 3, Some(5), 7, Some(110), DEFAULT_MARKERS)),
        score_query_match(&lower(skill.short_description.as_deref()), query, Bases::new(20, 22, Some(24), 26, None, DEFAULT_MARKERS)),
        score_query_match(&lower(skill.description.as_deref()), query, Bases::new(30, 32, Some(34), 36, None, DEFAULT_MARKERS)),
        score_query_match(&lower(skill.scope.as_deref()), query, Bases::new(40, 42, None, 44, None, DEFAULT_MARKERS)),
    ])
}

/// `searchProviderSkills`: the `$` menu.
pub fn search_skills(skills: &[ProviderSkill], query: &str) -> Vec<ProviderSkill> {
    let enabled = dedupe_by_name(skills.iter().filter(|skill| is_user_invocable(skill)).cloned());
    let query = normalize_search_query(query, is_currency_symbol);
    if query.is_empty() {
        return enabled;
    }
    rank(
        enabled
            .into_iter()
            .filter_map(|skill| {
                let score = score_skill(&skill, &query)?;
                let tie = format!("{}\u{0}{}", skill_display_name(&skill).to_lowercase(), skill.name);
                Some((skill, score, tie))
            })
            .collect(),
    )
}

// --- Menu items (`useComposerMenuItems.ts`) ---

/// `ComposerSlashCommand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    Model,
    Plan,
    Default,
    Side,
    Btw,
}

impl SlashCommand {
    pub fn name(self) -> &'static str {
        match self {
            SlashCommand::Model => "model",
            SlashCommand::Plan => "plan",
            SlashCommand::Default => "default",
            SlashCommand::Side => "side",
            SlashCommand::Btw => "btw",
        }
    }
}

/// `ProjectEntry` from `projects.searchEntries`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PathEntry {
    pub path: String,
    #[serde(default)]
    pub kind: PathKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    #[default]
    File,
    Directory,
}

/// `projects.searchEntries` for the `@` menu.
pub fn search_entries_request(cwd: &str, query: &str) -> Value {
    json!({ "cwd": cwd, "query": query.trim(), "limit": PATH_SEARCH_LIMIT })
}

/// The entries of a `projects.searchEntries` reply; malformed rows are skipped.
pub fn decode_entries(reply: &Value) -> Vec<PathEntry> {
    reply
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(|entry| serde_json::from_value(entry.clone()).ok()).collect())
        .unwrap_or_default()
}

/// What a row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    Path { path: String, kind: PathKind },
    Slash(SlashCommand),
    ProviderCommand { name: String },
    Skill { name: String, source: SkillSource },
    Thread { thread_id: String, title: String },
    Environment { environment_id: String, label: String },
}

/// `ComposerCommandItem`: one row.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub action: MenuAction,
}

/// What the `/` menu offers besides the provider's commands and skills.
#[derive(Debug, Clone, Copy, Default)]
pub struct SlashMenuOptions {
    pub plan_mode: bool,
    pub side_chat: bool,
    /// `sideChatDisabledReason`: replaces the `/side` and `/btw` descriptions.
    pub side_chat_disabled: Option<&'static str>,
    /// `compactSlashCommandAvailable`: `/compact` shows only when it could run now.
    pub compact: bool,
    pub show_skills: bool,
}

fn builtin_item(command: SlashCommand, description: &str) -> MenuItem {
    MenuItem {
        id: format!("slash:{}", command.name()),
        label: format!("/{}", command.name()),
        description: description.to_owned(),
        action: MenuAction::Slash(command),
    }
}

fn skill_item(driver: &str, skill: &ProviderSkill, label: String, fallback: &str) -> MenuItem {
    let description = skill
        .short_description
        .clone()
        .or_else(|| skill.description.clone())
        .unwrap_or_else(|| skill.scope.as_ref().map_or_else(|| fallback.to_owned(), |scope| format!("{scope} skill")));
    MenuItem {
        id: format!("skill:{driver}:{}", skill.name),
        label,
        description,
        action: MenuAction::Skill { name: skill.name.clone(), source: skill_source(skill) },
    }
}

/// `scoreSlashCommandItem`.
fn score_slash_item(item: &MenuItem, skills: &[ProviderSkill], query: &str) -> Option<i64> {
    let primary = match &item.action {
        MenuAction::Skill { name, .. } => {
            if query == "skill" {
                return Some(0);
            }
            let skill_query = query.strip_prefix("skill:").unwrap_or(query);
            let score = match skills.iter().find(|skill| skill.name == *name) {
                Some(skill) if !skill_query.is_empty() => score_skill(skill, skill_query),
                _ => Some(0),
            };
            return score.or_else(|| "skill".starts_with(query).then_some(i64::MAX));
        }
        MenuAction::Slash(command) => command.name().to_lowercase(),
        MenuAction::ProviderCommand { name } => name.to_lowercase(),
        _ => return None,
    };
    best_score([
        score_query_match(&primary, query, Bases::new(0, 2, Some(4), 6, Some(100), WORD_MARKERS)),
        score_query_match(&item.description.to_lowercase(), query, Bases::new(20, 22, Some(24), 26, None, DEFAULT_MARKERS)),
    ])
}

fn slash_tie_breaker(item: &MenuItem, driver: &str) -> String {
    match &item.action {
        MenuAction::Slash(command) => format!("0\u{0}{}", command.name()),
        MenuAction::ProviderCommand { name } => format!("1\u{0}{name}\u{0}{driver}"),
        MenuAction::Skill { name, .. } => format!("2\u{0}{name}\u{0}{driver}"),
        _ => String::new(),
    }
}

/// The `/` menu: built-ins, the provider's commands, then its skills as `/skill:name`, filtered
/// for where the token sits (`slashCommandItemsForPromptPosition`) and ranked by the query
/// (`searchSlashCommandItems`).
pub fn slash_items(
    trigger: &Trigger,
    driver: &str,
    skills: &[ProviderSkill],
    commands: &[ProviderSlashCommand],
    options: SlashMenuOptions,
) -> Vec<MenuItem> {
    let mut items = vec![builtin_item(SlashCommand::Model, "Switch response model for this thread")];
    if options.plan_mode {
        items.push(builtin_item(SlashCommand::Plan, "Switch this thread into plan mode"));
        items.push(builtin_item(SlashCommand::Default, "Switch this thread back to normal build mode"));
    }
    if options.side_chat {
        items.push(builtin_item(SlashCommand::Side, options.side_chat_disabled.unwrap_or("Ask a side question in a new side chat")));
        items.push(builtin_item(SlashCommand::Btw, options.side_chat_disabled.unwrap_or("Same as /side")));
    }
    let menu_skills =
        if options.show_skills { dedupe_by_name(skills.iter().filter(|skill| is_user_invocable(skill)).cloned()) } else { Vec::new() };
    let skill_names: std::collections::HashSet<String> = menu_skills.iter().map(|skill| skill.name.trim().to_lowercase()).collect();
    items.extend(
        commands
            .iter()
            .filter(|command| !skill_names.contains(&command.name.trim().to_lowercase()))
            .filter(|command| command.name != "compact" || options.compact)
            .map(|command| MenuItem {
                id: format!("provider-slash-command:{driver}:{}", command.name),
                label: format!("/{}", command.name),
                description: command
                    .description
                    .clone()
                    .or_else(|| command.input.as_ref().map(|input| input.hint.clone()))
                    .unwrap_or_else(|| "Run provider command".to_owned()),
                action: MenuAction::ProviderCommand { name: command.name.clone() },
            }),
    );
    items.extend(menu_skills.iter().map(|skill| skill_item(driver, skill, format!("/skill:{}", skill.name), "")));
    if trigger.range_start != 0 {
        items.retain(|item| match &item.action {
            MenuAction::Skill { .. } => true,
            MenuAction::Slash(command) => !matches!(command, SlashCommand::Side | SlashCommand::Btw),
            _ => false,
        });
    }
    let query = normalize_search_query(&trigger.query, |c| c == '/');
    if query.is_empty() {
        return items;
    }
    rank(
        items
            .into_iter()
            .filter_map(|item| {
                let score = score_slash_item(&item, &menu_skills, &query)?;
                let tie = slash_tie_breaker(&item, driver);
                Some((item, score, tie))
            })
            .collect(),
    )
}

/// The `$` menu.
pub fn skill_items(trigger: &Trigger, driver: &str, skills: &[ProviderSkill]) -> Vec<MenuItem> {
    search_skills(skills, &trigger.query)
        .iter()
        .map(|skill| skill_item(driver, skill, skill_display_name(skill).to_owned(), "Run provider skill"))
        .collect()
}

/// `matchComposerThreadItems`: up to five threads for `@`, ranked like the palette, never the
/// open one or an archived or hidden one. Bare `@` stays a file picker.
pub fn thread_items(threads: &[PaletteThread], environment_id: &str, exclude: Option<&str>, query: &str, now: i64) -> Vec<MenuItem> {
    let query = fold_search_query(query);
    if query.is_empty() {
        return Vec::new();
    }
    let candidates: Vec<(&PaletteThread, crate::search::ThreadSearchSubject)> = threads
        .iter()
        .filter(|thread| Some(thread.id.as_str()) != exclude && thread.archived_at.is_none() && !thread.hidden)
        .map(|thread| (thread, thread_search_subject(thread, None, None)))
        .collect();
    rank_search_items(&candidates, |(_, subject), typos| {
        score_thread_search(&query, subject, now, ThreadSearchOptions { typos, picks: false })
    })
    .into_iter()
    .take(THREAD_RESULT_LIMIT)
    .map(|ranked| {
        let thread = ranked.item.0;
        MenuItem {
            id: format!("thread:{environment_id}:{}", thread.id),
            label: thread.title.clone(),
            description: "Thread".to_owned(),
            action: MenuAction::Thread { thread_id: thread.id.clone(), title: thread.title.clone() },
        }
    })
    .collect()
}

/// The workspace rows of the `@` menu, in the server's order.
pub fn path_items(entries: &[PathEntry]) -> Vec<MenuItem> {
    entries
        .iter()
        .map(|entry| MenuItem {
            id: format!("path:{}:{}", if entry.kind == PathKind::Directory { "directory" } else { "file" }, entry.path),
            label: basename(&entry.path).to_owned(),
            description: entry.path.rsplit_once('/').map(|(parent, _)| parent.to_owned()).unwrap_or_default(),
            action: MenuAction::Path { path: entry.path.clone(), kind: entry.kind },
        })
        .collect()
}

/// `activeThreadHasCompactableConversation`: something besides `/compact` turns to compact.
pub fn has_compactable_conversation(projection: &crate::thread::ThreadProjection) -> bool {
    projection.visible_turn_items.iter().any(|projected| match &projected.item.kind {
        crate::turn_item::TurnItemKind::UserMessage { text, attachments, .. } => {
            text.trim().to_lowercase() != "/compact" || !attachments.is_empty()
        }
        _ => false,
    })
}

/// The copy under an empty menu: what is loading, or why nothing matched.
pub fn empty_text(kind: TriggerKind, loading: bool) -> &'static str {
    match (kind, loading) {
        (TriggerKind::Skill | TriggerKind::SlashCommand, true) => "Searching workspace skills...",
        (TriggerKind::PullRequest, true) => "Finding pull request...",
        (TriggerKind::Path, true) => "Searching workspace files...",
        (TriggerKind::Skill, false) => "No skills found. Try / to browse provider commands.",
        (TriggerKind::Path, false) => "No matching files or folders.",
        (TriggerKind::PullRequest, false) => "Pull requests are not available for this project.",
        (TriggerKind::SlashCommand, false) => "No matching command.",
    }
}

/// `resolveComposerMenuActiveItemId`: the highlighted row while it is still listed for the same
/// query, else the first.
pub fn active_item<'a>(items: &'a [MenuItem], highlighted: Option<&str>, highlight_key: Option<&str>, search_key: Option<&str>) -> Option<&'a MenuItem> {
    if highlight_key == search_key
        && let Some(item) = highlighted.and_then(|id| items.iter().find(|item| item.id == id))
    {
        return Some(item);
    }
    items.first()
}

/// `nudgeComposerMenuHighlight`: Up and Down wrap around.
pub fn nudge(items: &[MenuItem], highlighted: Option<&str>, down: bool) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let count = items.len() as isize;
    let current = highlighted.and_then(|id| items.iter().position(|item| item.id == id)).map(|index| index as isize);
    let normalized = current.unwrap_or(if down { -1 } else { 0 });
    let next = (normalized + if down { 1 } else { -1 } + count).rem_euclid(count);
    Some(items[next as usize].id.clone())
}

/// The key that keeps a highlight while the same menu shows the same query.
pub fn search_key(trigger: &Trigger) -> String {
    format!("{:?}:{}", trigger.kind, trigger.query.trim().to_lowercase())
}

// --- Thread references (`composerContextReferences.ts`) ---

pub const CONTEXT_HREF_PREFIX: &str = "t3-context://v1/";
pub const CONTEXT_LABEL_MAX_CHARS: usize = 200;

/// The digest that tells apart producer ids sharing a truncated slug.
fn fnv1a64(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let (mut forward, mut reverse) = (0x811c_9dc5_u32, 0x9dc5_811c_u32);
    for index in 0..units.len() {
        forward ^= u32::from(units[index]);
        forward = forward.wrapping_mul(0x0100_0193);
        reverse ^= u32::from(units[units.len() - 1 - index]);
        reverse = reverse.wrapping_mul(0x0100_0193);
    }
    format!("{forward:08x}{reverse:08x}")
}

pub fn is_context_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// `toComposerContextId`.
pub fn to_context_id(producer_id: &str) -> String {
    if is_context_id(producer_id) {
        return producer_id.to_owned();
    }
    let mut slug = String::new();
    let mut dash = false;
    for c in producer_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            slug.push(c);
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    format!("{}-{}", if slug.is_empty() { "ctx" } else { &slug }, fnv1a64(producer_id))
}

/// `toKindScopedComposerContextId`.
pub fn kind_scoped_context_id(kind: &str, producer_id: &str) -> String {
    to_context_id(&format!("{kind}_{producer_id}"))
}

/// `sanitizeComposerContextLabel`.
pub fn sanitize_context_label(label: &str, kind: &str) -> String {
    let cleaned: String = label.chars().map(|c| if matches!(c, '[' | ']' | '\\' | '\r' | '\n') { ' ' } else { c }).collect();
    let cleaned: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(CONTEXT_LABEL_MAX_CHARS).collect();
    if cleaned.is_empty() { kind.to_owned() } else { cleaned }
}

/// `ThreadContextRecord`: a thread the message references, as `threadContextRecord` builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadContext {
    pub context_id: String,
    pub label: String,
    pub environment_id: String,
    pub thread_id: String,
}

impl ThreadContext {
    pub fn new(environment_id: &str, thread_id: &str, title: &str) -> ThreadContext {
        ThreadContext {
            context_id: kind_scoped_context_id("thread", thread_id),
            label: sanitize_context_label(title, "thread"),
            environment_id: environment_id.to_owned(),
            thread_id: thread_id.to_owned(),
        }
    }

    /// `formatComposerContextReference`: the inline link the prompt carries.
    pub fn reference(&self) -> String {
        format!("[{}]({CONTEXT_HREF_PREFIX}thread/{})", self.label, self.context_id)
    }

    pub fn to_record(&self) -> Value {
        json!({
            "version": 1,
            "kind": "thread",
            "contextId": self.context_id,
            "label": self.label,
            "environmentId": self.environment_id,
            "threadId": self.thread_id,
            "title": self.label,
        })
    }
}

/// One inline `[label](t3-context://v1/kind/id)` link (`ComposerContextReferenceOccurrence`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReference<'a> {
    pub kind: &'a str,
    pub context_id: &'a str,
    pub label: &'a str,
    /// Written `![label](…)`.
    pub image: bool,
    pub start: usize,
    pub end: usize,
}

fn is_context_kind(kind: &str) -> bool {
    let bytes = kind.as_bytes();
    (1..=40).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// `collectComposerContextReferences`: the links `CONTEXT_LINK` matches, in order.
pub fn context_references(text: &str) -> Vec<ContextReference<'_>> {
    let marker = format!("]({CONTEXT_HREF_PREFIX}");
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find(&marker) {
        let close = from + offset;
        from = close + marker.len();
        let href_start = close + 2;
        let rest = &text[href_start..];
        let Some(href_len) = rest.find(|c: char| c == ')' || c.is_whitespace()) else { break };
        let href = &rest[..href_len];
        if !rest[href_len..].starts_with(')') || !(1..=200).contains(&href.chars().count()) {
            continue;
        }
        // The leftmost `[` the regex would start at: after the last `]` or newline, label ≤ 512.
        let floor = text[..close].rfind([']', '\n']).map_or(0, |at| at + 1);
        let floor = floor.max(found.last().map_or(0, |previous: &ContextReference| previous.end));
        let Some(open) = text[floor..close]
            .match_indices('[')
            .map(|(at, _)| floor + at)
            .find(|open| text[open + 1..close].chars().count() <= 512)
        else {
            continue;
        };
        let label = &text[open + 1..close];
        let Some((kind, context_id)) = href[CONTEXT_HREF_PREFIX.len()..].split_once('/') else { continue };
        if !is_context_kind(kind) || !is_context_id(context_id) {
            continue;
        }
        let image = open > floor && text[..open].ends_with('!');
        let start = if image { open - 1 } else { open };
        found.push(ContextReference { kind, context_id, label, image, start, end: href_start + href_len + 1 });
        from = href_start + href_len + 1;
    }
    found
}

/// The context ids the prompt still references (`collectInlineContextIds`), so a record whose
/// link the user deleted is not sent.
pub fn referenced_context_ids(text: &str) -> Vec<&str> {
    context_references(text).into_iter().map(|reference| reference.context_id).collect()
}

/// `threadTitleTextFromPrompt`: links read as their labels; images drop out, and a prompt of
/// only images reads "Attached image(s)".
pub fn title_text(text: &str) -> String {
    let mut prose = String::new();
    let mut images = std::collections::HashSet::new();
    let mut cursor = 0;
    for reference in context_references(text) {
        prose.push_str(&text[cursor..reference.start]);
        if reference.kind == "image" {
            images.insert(reference.context_id);
        } else {
            prose.push_str(&sanitize_context_label(reference.label, reference.kind));
        }
        cursor = reference.end;
    }
    prose.push_str(&text[cursor..]);
    let normalized = prose.split_whitespace().collect::<Vec<_>>().join(" ");
    match (normalized.is_empty(), images.len()) {
        (false, _) | (true, 0) => normalized,
        (true, 1) => "Attached image".to_owned(),
        (true, _) => "Attached images".to_owned(),
    }
}

/// `buildMessageContext` for thread references alone.
pub fn message_context(text: &str, threads: &[ThreadContext]) -> Option<Value> {
    super::attachments::message_context(text, threads, &[], &[])
}

#[cfg(test)]
mod tests;
