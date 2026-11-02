//! Port of the ranking and grouping in `apps/web/src/components/CommandPalette.logic.ts`, with `threadSearchSubject` from `packages/client-runtime/src/state/threadSearch.ts` and the recent-threads order from `CommandPalette.tsx` (`shortcuts/commandPaletteCommands.ts` holds only key handlers).
//!
//! Skipped: `ITEM_ICON_CLASS`, `ADDON_ICON_CLASS`, `buildLinkedThreadActionItems`, `browseInputEndPaddingClass`, `SearchOverlayMode`, `CommandPaletteOpenIntent`, `CommandPaletteUiState`, `CommandPaletteUiAction`, `reduceCommandPaletteUiState`, `AddProjectEntry`, `resolveAddProjectEntry`, `CommandPaletteThreadContentMatch`, `CommandPaletteItemPin`, `CommandPaletteReturnContext`, `buildCommandPaletteProjectMetadata`, `buildBrowseGroups`, `filterPinnedBrowseEntries`, `getEnvironmentBrowsePlatform`, `AddProjectEnvironmentOption`, `AddProjectCloneFlow`, `remoteProjectInputPlaceholder`, `buildPaletteRemoteSourceReadiness`, `errorMessage`, `overlayModeForCommand`, `resolveCommandPaletteProject`, `newThreadNoProjectIndex`, and the `threadMessageItems` ("In messages") input of `filterCommandPaletteGroups`.

use std::collections::{HashMap, HashSet};

use crate::search::{
    PALETTE_SECONDARY_LIMIT, PALETTE_THREAD_LIMIT, RankedGroup, RankedItem, SearchMatch, SearchMatchHint,
    ThreadOpenStats, ThreadPullRequest, ThreadSearchOptions, ThreadSearchSubject, fold_search_query,
    rank_search_groups, score_search_terms, score_thread_search, thread_recent_use,
};

pub const RECENT_THREAD_LIMIT: usize = 12;
pub const THREAD_JUMP_KEYBINDING_COMMANDS: [&str; 9] = [
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

#[derive(Debug, Clone, Default)]
pub enum CommandPaletteItemKind {
    #[default]
    Action,
    Submenu(CommandPaletteView),
}

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteItem {
    pub kind: CommandPaletteItemKind,
    pub value: String,
    pub title: String,
    pub description: Option<String>,
    /// Keywords; the title is searched too.
    pub search_terms: Vec<String>,
    /// The ISO time the row's relative time label shows.
    pub timestamp: Option<String>,
    /// Thread rows rank by this, with use history, instead of by `search_terms`.
    pub thread_search: Option<ThreadSearchSubject>,
    /// Shown in place of the description when the match is not in the title.
    pub match_hint: Option<SearchMatchHint>,
    /// The keybinding command whose shortcut the row shows, such as `thread.jump.1`.
    pub shortcut_command: Option<String>,
    pub disabled: bool,
    /// Listed last in its group and never decides where the group sits.
    pub secondary: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteGroup {
    pub value: String,
    pub label: String,
    pub items: Vec<CommandPaletteItem>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteView {
    pub groups: Vec<CommandPaletteGroup>,
    pub initial_query: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PushedView {
    pub view: CommandPaletteView,
    pub parent_query: String,
}

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteSearchState {
    pub query: String,
    pub views: Vec<PushedView>,
}

#[derive(Debug, Clone)]
pub enum CommandPaletteSearchAction {
    Query(String),
    Push(CommandPaletteView),
    Back,
    Reset(Option<String>),
}

pub fn create_command_palette_search_state(query: &str) -> CommandPaletteSearchState {
    CommandPaletteSearchState { query: query.to_owned(), views: Vec::new() }
}

pub fn reduce_command_palette_search_state(
    state: CommandPaletteSearchState,
    action: CommandPaletteSearchAction,
) -> CommandPaletteSearchState {
    let CommandPaletteSearchState { query, mut views } = state;
    match action {
        CommandPaletteSearchAction::Query(query) => CommandPaletteSearchState { query, views },
        CommandPaletteSearchAction::Push(view) => {
            let next = view.initial_query.clone().unwrap_or_default();
            views.push(PushedView { view, parent_query: query });
            CommandPaletteSearchState { query: next, views }
        }
        CommandPaletteSearchAction::Back => match views.pop() {
            Some(parent) => CommandPaletteSearchState { query: parent.parent_query, views },
            None => CommandPaletteSearchState { query, views },
        },
        CommandPaletteSearchAction::Reset(query) => {
            create_command_palette_search_state(query.as_deref().unwrap_or(""))
        }
    }
}

#[derive(Debug, Clone)]
pub enum CommandPaletteRow<'a> {
    Label { key: String, label: &'a str, first: bool },
    Item { key: String, item: &'a CommandPaletteItem, item_index: Option<usize> },
}

/// Rows to render, plus the keyboard order: enabled item values and the row each one sits on.
#[derive(Debug, Clone)]
pub struct CommandPaletteRows<'a> {
    pub rows: Vec<CommandPaletteRow<'a>>,
    pub item_values: Vec<&'a str>,
    pub row_index_by_item_index: Vec<usize>,
}

pub fn build_command_palette_rows(groups: &[CommandPaletteGroup]) -> CommandPaletteRows<'_> {
    let mut built =
        CommandPaletteRows { rows: Vec::new(), item_values: Vec::new(), row_index_by_item_index: Vec::new() };
    for group in groups {
        if !group.label.is_empty() {
            let first = built.rows.is_empty();
            built.rows.push(CommandPaletteRow::Label {
                key: format!("group:{}", group.value),
                label: &group.label,
                first,
            });
        }
        for item in &group.items {
            let item_index = (!item.disabled).then_some(built.item_values.len());
            if item_index.is_some() {
                built.item_values.push(&item.value);
                built.row_index_by_item_index.push(built.rows.len());
            }
            let key = format!("{}:{}", group.value, item.value);
            built.rows.push(CommandPaletteRow::Item { key, item, item_index });
        }
    }
    built
}

pub fn find_highlighted_command_palette_item<'a>(
    groups: &'a [CommandPaletteGroup],
    highlighted_item_value: Option<&str>,
) -> Option<&'a CommandPaletteItem> {
    let value = highlighted_item_value?;
    groups.iter().flat_map(|group| &group.items).find(|item| item.value == value && !item.disabled)
}

/// Gives the first nine rows the thread jump shortcuts and clears the rest.
pub fn enumerate_command_palette_items(items: Vec<CommandPaletteItem>) -> Vec<CommandPaletteItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| CommandPaletteItem {
            shortcut_command: THREAD_JUMP_KEYBINDING_COMMANDS.get(index).map(|command| (*command).to_owned()),
            ..item
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPaletteMode {
    Root,
    RootBrowse,
    Submenu,
    SubmenuBrowse,
}

pub fn get_command_palette_mode(has_current_view: bool, is_browsing: bool) -> CommandPaletteMode {
    match (has_current_view, is_browsing) {
        (true, true) => CommandPaletteMode::SubmenuBrowse,
        (true, false) => CommandPaletteMode::Submenu,
        (false, true) => CommandPaletteMode::RootBrowse,
        (false, false) => CommandPaletteMode::Root,
    }
}

pub fn get_command_palette_input_placeholder(mode: CommandPaletteMode) -> &'static str {
    match mode {
        CommandPaletteMode::Root => "Search commands, projects, and threads...",
        CommandPaletteMode::RootBrowse => "Enter project path (e.g. ~/projects/my-app)",
        CommandPaletteMode::Submenu => "Search...",
        CommandPaletteMode::SubmenuBrowse => "Enter path (e.g. ~/projects/my-app)",
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteProject {
    pub environment_id: String,
    pub id: String,
    pub title: String,
    pub display_name: String,
    pub workspace_root: String,
}

pub fn build_project_action_items(
    projects: &[CommandPaletteProject],
    value_prefix: &str,
    extra_search_terms: impl Fn(&CommandPaletteProject) -> Vec<String>,
    shortcut_command: Option<&str>,
) -> Vec<CommandPaletteItem> {
    projects
        .iter()
        .map(|project| {
            let mut search_terms =
                vec![project.display_name.clone(), project.title.clone(), project.workspace_root.clone()];
            search_terms.extend(extra_search_terms(project));
            CommandPaletteItem {
                value: format!("{value_prefix}:{}:{}", project.environment_id, project.id),
                title: project.display_name.clone(),
                description: Some(project.workspace_root.clone()),
                search_terms,
                shortcut_command: shortcut_command.map(str::to_owned),
                ..Default::default()
            }
        })
        .collect()
}

/// The thread fields the palette lists and ranks by.
#[derive(Debug, Clone, Default)]
pub struct PaletteThread {
    pub environment_id: String,
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub branch: Option<String>,
    pub archived_at: Option<String>,
    /// Subagent and side-chat threads, which no thread list shows.
    pub hidden: bool,
    pub created_at: String,
    pub updated_at: String,
    pub latest_user_message_at: Option<String>,
    /// `None` when the server omits the field (older servers); `Some(None)` when the user never wrote.
    pub latest_user_authored_message_at: Option<Option<String>>,
    pub pull_requests: Vec<ThreadPullRequest>,
    pub opens: Option<ThreadOpenStats>,
    /// This client's latest open, which the server may have merged away or not recorded yet.
    pub local_opened_at: Option<i64>,
}

pub fn thread_search_subject(
    thread: &PaletteThread,
    project_title: Option<&str>,
    environment_label: Option<String>,
) -> ThreadSearchSubject {
    // What the user wrote; an older server omits it, so the latest user-role message stands in.
    let activity_at = match &thread.latest_user_authored_message_at {
        Some(authored) => parse_iso_ms(authored.as_deref().unwrap_or(&thread.created_at)),
        None => {
            [thread.latest_user_message_at.as_deref(), Some(&thread.updated_at), Some(&thread.created_at)]
                .into_iter()
                .flatten()
                .find_map(parse_iso_ms)
        }
    };
    let local_opened_at = thread.local_opened_at.unwrap_or(0);
    let opens = match &thread.opens {
        _ if local_opened_at <= thread.opens.as_ref().map_or(0, |opens| opens.opened_at) => {
            thread.opens.clone()
        }
        Some(opens) => Some(ThreadOpenStats { opened_at: local_opened_at, ..opens.clone() }),
        None => Some(ThreadOpenStats { opened_at: local_opened_at, score: 1.0, picks: Vec::new() }),
    };
    ThreadSearchSubject {
        id: Some(thread.id.clone()),
        title: thread.title.clone(),
        branch: thread.branch.clone(),
        project_title: project_title.map(str::to_owned),
        environment_label,
        pull_requests: thread.pull_requests.clone(),
        activity_at,
        opens,
        prepared: Default::default(),
    }
}

/// Thread rows in the given order. `active_thread` is `(environment_id, thread_id)`;
/// `project_titles` is keyed `"{environment_id}:{project_id}"`. Pass an environment label only
/// with two or more environments.
pub fn build_thread_action_items(
    threads: &[PaletteThread],
    active_thread: Option<(&str, &str)>,
    project_titles: &HashMap<String, String>,
    environment_label_of: impl Fn(&PaletteThread) -> Option<String>,
) -> Vec<CommandPaletteItem> {
    threads
        .iter()
        .filter(|thread| thread.archived_at.is_none() && !thread.hidden)
        .map(|thread| {
            let project_title = project_titles
                .get(&format!("{}:{}", thread.environment_id, thread.project_id))
                .map(String::as_str)
                .filter(|title| !title.is_empty());
            let is_current = active_thread == Some((thread.environment_id.as_str(), thread.id.as_str()));
            let branch = thread
                .branch
                .as_deref()
                .filter(|branch| !branch.is_empty())
                .map(|branch| format!("#{branch}"));
            let parts: Vec<String> =
                [project_title.map(str::to_owned), branch, is_current.then(|| "Current thread".into())]
                    .into_iter()
                    .flatten()
                    .collect();
            CommandPaletteItem {
                value: format!("thread:{}:{}", thread.environment_id, thread.id),
                title: thread.title.clone(),
                description: (!parts.is_empty()).then(|| parts.join(" · ")),
                search_terms: vec![thread.title.clone()],
                timestamp: Some(
                    thread.latest_user_message_at.clone().unwrap_or_else(|| thread.updated_at.clone()),
                ),
                thread_search: Some(thread_search_subject(
                    thread,
                    project_title,
                    environment_label_of(thread),
                )),
                // Last in its group: picking the thread already open does nothing.
                secondary: is_current,
                ..Default::default()
            }
        })
        .collect()
}

/// Recent means used recently: written in or opened, whichever is later. The open thread would
/// always lead, so it is left out.
pub fn recent_thread_items(
    thread_items: &[CommandPaletteItem],
    active_thread_value: Option<&str>,
) -> Vec<CommandPaletteItem> {
    let mut recent: Vec<&CommandPaletteItem> =
        thread_items.iter().filter(|item| Some(item.value.as_str()) != active_thread_value).collect();
    recent.sort_by_key(|item| std::cmp::Reverse(item.thread_search.as_ref().map_or(0, thread_recent_use)));
    recent.into_iter().take(RECENT_THREAD_LIMIT).cloned().collect()
}

pub fn build_root_groups(
    action_items: Vec<CommandPaletteItem>,
    recent_thread_items: Vec<CommandPaletteItem>,
) -> Vec<CommandPaletteGroup> {
    [("actions", "Actions", action_items), ("recent-threads", "Recent Threads", recent_thread_items)]
        .into_iter()
        .filter(|(_, _, items)| !items.is_empty())
        .map(|(value, label, items)| CommandPaletteGroup { value: value.into(), label: label.into(), items })
        .collect()
}

/// Every option inside a submenu as one row, e.g. "New thread in t3code" or "Change theme: Dracula".
pub fn build_nested_search_items(items: &[CommandPaletteItem], query: &str) -> Vec<CommandPaletteItem> {
    let top_level_names: HashSet<&str> = items.iter().map(|item| item.title.as_str()).collect();
    let folded_query = fold_search_query(query);
    let mut nested = Vec::new();
    for parent in items {
        let CommandPaletteItemKind::Submenu(view) = &parent.kind else { continue };
        let stem = parent
            .title
            .strip_suffix("...")
            .or_else(|| parent.title.strip_suffix('…'))
            .unwrap_or(&parent.title);
        // A query that only names the submenu ("new thread in") wants the submenu row itself.
        if fold_search_query(stem).starts_with(&folded_query) {
            continue;
        }
        let prefix = if stem == parent.title { format!("{stem}: ") } else { format!("{} ", stem.trim_end()) };
        for child in view.groups.iter().flat_map(|group| &group.items) {
            let title = format!("{prefix}{}", child.title);
            if matches!(child.kind, CommandPaletteItemKind::Action)
                && !top_level_names.contains(title.as_str())
            {
                // Only the combined title: child keywords like "theme" would list every theme.
                nested.push(CommandPaletteItem { search_terms: vec![title.clone()], title, ..child.clone() });
            }
        }
    }
    nested
}

/// What `filter_command_palette_groups` searches. `now` is epoch ms.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandPaletteSearch<'a> {
    pub active_groups: &'a [CommandPaletteGroup],
    pub query: &'a str,
    pub is_in_submenu: bool,
    /// Submenu options join the root search, and a submenu with no match falls back to it.
    pub all_options: Option<CommandPaletteAllOptions<'a>>,
    pub project_search_items: &'a [CommandPaletteItem],
    pub settings_search_items: &'a [CommandPaletteItem],
    pub thread_search_items: &'a [CommandPaletteItem],
    pub now: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CommandPaletteAllOptions<'a> {
    pub root_groups: &'a [CommandPaletteGroup],
    pub nested_items: &'a [CommandPaletteItem],
}

struct SearchableGroup<'a> {
    value: &'a str,
    label: &'a str,
    items: &'a [CommandPaletteItem],
}

fn score_palette_item(
    item: &CommandPaletteItem,
    normalized_query: &str,
    now: i64,
    typos: bool,
) -> Option<SearchMatch> {
    let mut found = match &item.thread_search {
        Some(subject) => {
            score_thread_search(normalized_query, subject, now, ThreadSearchOptions { typos, picks: true })
        }
        // The title names the item; search terms are keywords.
        None if item.search_terms.first() == Some(&item.title) => {
            score_search_terms(normalized_query, &item.search_terms, typos)
        }
        None => {
            let terms: Vec<&str> = std::iter::once(item.title.as_str())
                .chain(item.search_terms.iter().map(String::as_str))
                .collect();
            score_search_terms(normalized_query, &terms, typos)
        }
    }?;
    // A row listed last in its group (a keybinding setting, the open thread) does not place the group.
    if item.secondary {
        found.group_quality = Some(0.0);
    }
    Some(found)
}

/// Equal match classes put commands before threads, so "new" runs "New thread". A leading ">"
/// searches only actions.
pub fn filter_command_palette_groups(input: &CommandPaletteSearch<'_>) -> Vec<CommandPaletteGroup> {
    if let (Some(all_options), true) = (input.all_options, input.is_in_submenu) {
        let scoped = filter_command_palette_groups(&CommandPaletteSearch { all_options: None, ..*input });
        if !scoped.is_empty() || input.query.trim().is_empty() {
            return scoped;
        }
        let root =
            CommandPaletteSearch { active_groups: all_options.root_groups, is_in_submenu: false, ..*input };
        return filter_command_palette_groups(&root);
    }
    let actions_only = input.query.starts_with('>');
    let normalized_query = fold_search_query(if actions_only { &input.query[1..] } else { input.query });
    if normalized_query.is_empty() {
        let groups = input.active_groups.iter().filter(|group| !actions_only || group.value == "actions");
        return groups.cloned().collect();
    }
    let searches_everything = !input.is_in_submenu && !actions_only;
    let mut searchable: Vec<SearchableGroup<'_>> = input
        .active_groups
        .iter()
        .filter(|group| {
            if actions_only {
                group.value == "actions"
            } else {
                input.is_in_submenu || group.value != "recent-threads"
            }
        })
        .map(|group| SearchableGroup { value: &group.value, label: &group.label, items: &group.items })
        .collect();
    // On equal match classes, groups keep this order: threads, what people search for most, right after commands.
    if searches_everything {
        let extra = [
            ("nested-search", "Suggestions", input.all_options.map_or(&[][..], |all| all.nested_items)),
            ("threads-search", "Threads", input.thread_search_items),
            ("projects-search", "Projects", input.project_search_items),
            ("settings-search", "Settings", input.settings_search_items),
        ];
        let extra = extra.into_iter().filter(|(_, _, items)| !items.is_empty());
        searchable.extend(extra.map(|(value, label, items)| SearchableGroup { value, label, items }));
    }
    let ranked = rank_search_groups(
        &searchable,
        |group| group.items,
        |item, typos, _| score_palette_item(item, &normalized_query, input.now, typos),
    );
    let threads_match = ranked.iter().any(|entry| entry.group.value == "threads-search");
    ranked
        .into_iter()
        .map(|RankedGroup { group, mut ranked }| {
            let limit = if group.value == "threads-search" {
                PALETTE_THREAD_LIMIT
            } else if group.value == "nested-search" || (searches_everything && threads_match) {
                PALETTE_SECONDARY_LIMIT
            } else {
                ranked.len()
            };
            // Secondary items stay below the rest; the sort is stable, so score order holds.
            ranked.sort_by_key(|entry| entry.item.secondary);
            let items = ranked.into_iter().take(limit).map(|RankedItem { item, matched }| {
                // Hints are built only for the rows shown.
                let hint = match (&item.thread_search, matched.ranges.is_empty()) {
                    (Some(subject), true) => matched.explain(subject),
                    _ => None,
                };
                CommandPaletteItem { match_hint: hint.or_else(|| item.match_hint.clone()), ..item.clone() }
            });
            CommandPaletteGroup {
                value: group.value.to_owned(),
                label: group.label.to_owned(),
                items: items.collect(),
            }
        })
        .collect()
}

/// `YYYY-MM-DDTHH:MM:SS(.fff)` with `Z` or `±HH:MM`, to epoch ms.
fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = bytes.get(from..to)?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if separators.iter().any(|&(at, expected)| bytes.get(at) != Some(&expected)) {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let mut at = 19;
    let mut millis = 0;
    if bytes.get(at) == Some(&b'.') {
        let start = at + 1;
        at = start;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
        millis =
            (start..start + 3).fold(0, |ms, i| ms * 10 + number(i, i + 1).filter(|_| i < at).unwrap_or(0));
    }
    let offset_minutes = match bytes.get(at..)? {
        b"Z" => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (hours, minutes) = (number(at + 1, at + 3)?, number(at + 4, at + 6)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 60 + minutes) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let days_in_month = usize::try_from(month - 1).ok().and_then(|m| month_days.get(m))?;
    if day < 1 || day > *days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days =
        era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60) * 1000 + millis)
}

#[cfg(test)]
mod tests;
