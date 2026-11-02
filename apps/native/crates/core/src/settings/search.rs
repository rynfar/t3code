//! Settings sections, their sidebar order, and settings search: `SETTINGS_SECTION_LABELS`,
//! `SETTINGS_NAV_GROUPS`, `SETTINGS_SEARCH_ITEMS` and `searchSettings` from
//! `web/src/components/settings/settingsSearch.ts`. Only the entries for rows the native app
//! renders are listed, so a result never lands on a page without its row.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    General,
    Appearance,
    Chat,
    Diffs,
    Keybindings,
    SnapShot,
    Providers,
    Threads,
    Browser,
    Devices,
    ScheduledTasks,
    Projects,
    SourceControl,
    Worktrees,
    Connections,
    Access,
    Secrets,
    Maintenance,
    Labs,
    About,
    Archived,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Appearance",
            Section::Chat => "Chat",
            Section::Diffs => "Diffs & pull requests",
            Section::Keybindings => "Keybindings",
            Section::SnapShot => "SnapShots",
            Section::Providers => "Providers",
            Section::Threads => "Threads",
            Section::Browser => "Browser",
            Section::Devices => "Devices",
            Section::ScheduledTasks => "Scheduled tasks",
            Section::Projects => "Projects",
            Section::SourceControl => "Source control",
            Section::Worktrees => "Worktrees",
            Section::Connections => "Environments",
            Section::Access => "Sharing & access",
            Section::Secrets => "Secrets",
            Section::Maintenance => "Maintenance",
            Section::Labs => "Labs",
            Section::About => "About",
            Section::Archived => "Archive",
        }
    }

    /// Sections the native app renders. The rest stay out of the sidebar until they port.
    pub fn is_available(self) -> bool {
        matches!(
            self,
            Section::General
                | Section::Appearance
                | Section::Chat
                | Section::Keybindings
                | Section::Providers
                | Section::Threads
                | Section::Worktrees
                | Section::Connections
                | Section::Labs
                | Section::About
                | Section::Archived
        )
    }
}

/// `SETTINGS_NAV_GROUPS`: a group without a heading is set apart by space alone.
pub const NAV_GROUPS: &[(Option<&str>, &[Section])] = &[
    (None, &[Section::General, Section::Appearance, Section::Chat, Section::Diffs, Section::Keybindings, Section::SnapShot]),
    (Some("Agents"), &[Section::Providers, Section::Threads, Section::Browser, Section::Devices, Section::ScheduledTasks]),
    (Some("Code"), &[Section::Projects, Section::SourceControl, Section::Worktrees]),
    (Some("Connections"), &[Section::Connections, Section::Access, Section::Secrets]),
    (None, &[Section::Maintenance, Section::Labs, Section::About, Section::Archived]),
];

/// The sidebar the native app shows: available sections, groups left empty dropped.
pub fn nav_groups() -> Vec<(Option<&'static str>, Vec<Section>)> {
    NAV_GROUPS
        .iter()
        .map(|(heading, sections)| (*heading, sections.iter().copied().filter(|section| section.is_available()).collect::<Vec<_>>()))
        .filter(|(_, sections)| !sections.is_empty())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchItem {
    pub id: String,
    pub title: String,
    pub section: Section,
    /// The row a result scrolls to when it differs from `id`.
    pub target_id: Option<String>,
    pub terms: Vec<String>,
    /// Keybinding results sort after every other match.
    pub secondary: bool,
}

impl SearchItem {
    fn new(id: &str, title: &str, section: Section, terms: &[&str]) -> SearchItem {
        SearchItem {
            id: id.to_owned(),
            title: title.to_owned(),
            section,
            target_id: None,
            terms: terms.iter().map(|term| (*term).to_owned()).collect(),
            secondary: false,
        }
    }

    fn target(mut self, target: &str) -> SearchItem {
        self.target_id = Some(target.to_owned());
        self
    }

    /// Where a jump lands: the target row, else the item's own row.
    pub fn anchor(&self) -> &str {
        self.target_id.as_deref().unwrap_or(&self.id)
    }
}

/// `SETTINGS_SEARCH_ITEMS` for the rows the native panels render, in the web's order.
pub fn items() -> Vec<SearchItem> {
    use Section::*;
    vec![
        SearchItem::new("storage-worktree-keep-when", "Keep worktrees with local changes", Worktrees, &["any local files uncommitted changes edited tracked files ignored env build output cleanup"])
            .target("storage-worktrees"),
        SearchItem::new("storage-worktrees", "Worktree cleanup", Worktrees, &["disk storage automatic delete deleted archived threads old inactive merged settled unchanged worktrees retention days project inherit off custom"]),
        SearchItem::new("storage-worktrees-location", "Worktree location", Worktrees, &["worktree location folder directory path drive external disk"]),
        SearchItem::new("project-defaults", "Project defaults and overrides", Threads, &["model workspace environments projects inheritance checkout"]),
        SearchItem::new("default-model", "Default model", Threads, &["new thread project provider reasoning effort"]),
        SearchItem::new("fallback-model", "Fallback model", Threads, &["fallback agent provider out of usage new threads"]),
        SearchItem::new("default-permissions", "Permissions", Threads, &["new thread default runtime mode supervised approvals auto accept edits full access"]),
        SearchItem::new("color-scheme", "Color scheme", Appearance, &["appearance light dark system mode"]).target("appearance"),
        SearchItem::new("theme", "Themes", Appearance, &["appearance colors palette custom import"]).target("appearance"),
        SearchItem::new("chat-width", "Chat width", Appearance, &["wide full width column layout messages composer monitor"]),
        SearchItem::new("interface-font", "Interface font", Appearance, &["typography family size system sans"]),
        SearchItem::new("project-grouping", "Project grouping", General, &["combine matching repositories environments sidebar"]),
        SearchItem::new("project-order", "Project order", General, &["sort projects sidebar manual created recent"]),
        SearchItem::new("snooze-options", "Snooze options", Threads, &["snooze presets times later tonight tomorrow next week remind"]),
        SearchItem::new("snooze-limited-threads", "Snooze until limits reset", Threads, &["usage quota rate limit reset wake recover continue"]),
        SearchItem::new("auto-resume-limited-threads", "Resume after usage limits", Threads, &["usage quota rate limit reset recover continue"]),
        SearchItem::new("working-shelf", "Working section", General, &["hide fold running monitoring threads inbox sidebar shelf"]),
        SearchItem::new("auto-settle-inactive-threads", "Auto-settle inactive threads", Threads, &["sidebar inactivity days no activity automatically"]),
        SearchItem::new("compact-idle-threads", "Compact idle threads", Providers, &["claude context compaction idle cache tokens usage summary resume"]),
        SearchItem::new("auto-settle-merged-threads", "Auto-settle merged threads", Threads, &["pull request merge closed automatically sidebar"]),
        SearchItem::new("keep-threads-with-pull-requests", "Keep threads with a PR", Threads, &["pull request inactivity auto-settle merge close sidebar"])
            .target("auto-settle-inactive-threads"),
        SearchItem::new("days-before-auto-settle", "Days of inactivity before auto-settle", Threads, &["thread timeout activity sidebar"])
            .target("auto-settle-inactive-threads"),
        SearchItem::new("thread-notifications", "Thread notifications", General, &["notification sound alert completion input approval desktop"]),
        SearchItem::new("in-app-notifications", "In-app notifications", General, &["notification toast popup completion input approval failure"]),
        SearchItem::new("time-format", "Time format", General, &["timestamp clock locale system browser os 12 hour 24 hour"]),
        SearchItem::new("response-streaming", "Response streaming", Threads, &["output token paragraph buffered wait turn legacy"]),
        SearchItem::new("send-shortcut", "Send shortcut", Chat, &["enter return command ctrl multiline prompt new line composer"]),
        SearchItem::new("follow-up-behavior", "Follow-up behavior", Chat, &["queue steer running turn send default behavior composer"]),
        SearchItem::new("new-threads", "New threads", Threads, &["default workspace mode draft local worktree"]),
        SearchItem::new("worktree-submodules", "Submodules", Worktrees, &["git submodule init recursive top-level none worktree t3.json"]),
        SearchItem::new("start-from-origin", "Start from remote", Worktrees, &["new worktrees start remote origin latest fetched branch local"]),
        SearchItem::new("add-project-starts-in", "Add project starts in", Threads, &["base directory folder browser path home"]),
        SearchItem::new("unpin-confirmation", "Unpin confirmation", General, &["ask before thread pinned section"]),
        SearchItem::new("archive-confirmation", "Archive confirmation", General, &["ask before thread second click inline action"]),
        SearchItem::new("delete-confirmation", "Delete confirmation", General, &["ask before thread chat history"]),
        SearchItem::new("quit-confirmation", "Quit shortcut", General, &["confirmation desktop app exit direct hold double click press twice"]),
        SearchItem::new("app-version", "Version", About, &["about check for updates download install upgrade release"]),
        SearchItem::new("legacy-plan-mode", "Plan mode (legacy)", Labs, &["build plan composer old"]),
        SearchItem::new("legacy-context-window-indicator", "Context window indicator (legacy)", Labs, &["composer meter usage tokens circle old"]),
        SearchItem::new("keybindings", "Keybindings", Keybindings, &["keyboard shortcuts hotkeys commands bindings json"]),
        SearchItem::new("providers", "Providers", Providers, &["agents cli codex claude cursor grok fx opencode antigravity google sign in sign out install subscription instances authentication api key models configuration binary path config directory codex_home shadow home endpoint arguments environment variables display name accent color custom favorite hidden auto compact native sessions import resume"]),
        SearchItem::new("provider-health-check-interval", "Health check interval", Providers, &["refresh availability versions auth state models background probes seconds off"]),
        SearchItem::new("worktree-branch-naming", "Worktree branch naming", Worktrees, &["static semantic prefix custom prompt instructions feat fix refactor chore"]),
        SearchItem::new("remote-environments", "Environments", Connections, &["add pair backend host code ssh config agent tunnel saved t3 connect"]),
        SearchItem::new("archive", "Archived threads", Archived, &["restore reopen deleted history projects"]),
    ]
}

/// `KEYBINDING_SEARCH_ITEMS`, from the commands the environment's keymap binds.
pub fn keybinding_item(command: &str, label: &str, keys: &[String]) -> SearchItem {
    let mut terms = vec![command.to_owned()];
    terms.extend(keys.iter().cloned());
    SearchItem {
        id: format!("keybinding-{command}"),
        title: label.to_owned(),
        section: Section::Keybindings,
        target_id: keys.is_empty().then(|| "keybindings".to_owned()),
        terms,
        secondary: true,
    }
}

/// `normalizeSearchText`: lowercase, combining marks dropped, whitespace collapsed.
pub fn normalize(value: &str) -> String {
    let stripped: String =
        value.chars().filter(|c| !('\u{0300}'..='\u{036f}').contains(c)).flat_map(char::to_lowercase).collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `searchSettings`: every query token must appear in the title, the section label, or a term.
/// Primary results first, then by rank (exact title, title prefix, title phrase, title tokens,
/// any phrase), then list order.
pub fn search<'a>(query: &str, items: &'a [SearchItem]) -> Vec<&'a SearchItem> {
    let query = normalize(query);
    if query.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = query.split(' ').collect();
    let mut ranked: Vec<(usize, u8, &SearchItem)> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let title = normalize(&item.title);
            let mut fields = vec![title.clone(), normalize(item.section.label())];
            fields.extend(item.terms.iter().map(|term| normalize(term)));
            if !tokens.iter().all(|token| fields.iter().any(|field| field.contains(token))) {
                return None;
            }
            let rank = if title == query {
                5
            } else if title.starts_with(&query) {
                4
            } else if title.contains(&query) {
                3
            } else if tokens.iter().all(|token| title.contains(token)) {
                2
            } else if fields.iter().any(|field| field.contains(&query)) {
                1
            } else {
                0
            };
            Some((index, rank, item))
        })
        .collect();
    ranked.sort_by(|(left_index, left_rank, left), (right_index, right_rank, right)| {
        left.secondary
            .cmp(&right.secondary)
            .then(right_rank.cmp(left_rank))
            .then(left_index.cmp(right_index))
    });
    ranked.into_iter().map(|(_, _, item)| item).collect()
}
