//! Settings sections, their sidebar order, and settings search: `SETTINGS_SECTION_LABELS`,
//! `SETTINGS_NAV_GROUPS`, `SETTINGS_SEARCH_ITEMS` and `searchSettings` from
//! `web/src/components/settings/settingsSearch.ts`. Only the entries for rows the native app
//! renders are listed, so a result never lands on a page without its row. Secrets ships behind
//! `T3CODE_FEATURE_SECRETS`, as `isSecretsFeatureEnabled` gates it on the web.

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
    /// A detail page Maintenance links to; the nav highlights Maintenance while it is open.
    Diagnostics,
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
            Section::Diagnostics => "Diagnostics",
        }
    }

    /// `SETTINGS_DETAIL_PAGE_PARENTS`: the nav entry a detail page highlights.
    pub fn nav_parent(self) -> Section {
        match self {
            Section::Diagnostics => Section::Maintenance,
            section => section,
        }
    }

    /// Sections the native app renders. The rest stay out of the sidebar until they port.
    pub fn is_available(self, features: Features) -> bool {
        (self == Section::Secrets && features.secrets)
            || matches!(
            self,
            Section::General
                | Section::Appearance
                | Section::Chat
                | Section::Diffs
                    | Section::Browser
                    | Section::SnapShot
                | Section::Keybindings
                | Section::Providers
                | Section::Threads
                | Section::Devices
                | Section::ScheduledTasks
                | Section::Projects
                | Section::SourceControl
                | Section::Worktrees
                | Section::Connections
                | Section::Access
                | Section::Maintenance
                | Section::Labs
                | Section::About
                | Section::Archived
                | Section::Diagnostics
            )
    }
}

/// Build-time features the web reads from its environment, read by the native app at launch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    pub secrets: bool,
}

impl Features {
    /// `isSecretsFeatureEnabled`: `T3CODE_FEATURE_SECRETS` set to `1` or `true`.
    pub fn from_secrets_flag(value: Option<&str>) -> Features {
        Features { secrets: value.is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true")) }
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
pub fn nav_groups(features: Features) -> Vec<(Option<&'static str>, Vec<Section>)> {
    NAV_GROUPS
        .iter()
        .map(|(heading, sections)| (*heading, sections.iter().copied().filter(|section| section.is_available(features)).collect::<Vec<_>>()))
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
pub fn items(features: Features) -> Vec<SearchItem> {
    use Section::*;
    let mut items = vec![
        SearchItem::new("diff-color-scheme", "Diff colors", Diffs, &["red green blue orange additions deletions changes counts palette colorblind"]),
        SearchItem::new("hide-whitespace-changes", "Hide whitespace changes", Diffs, &["diff ignore spaces edits default"]),
        SearchItem::new("default-diff-file-state", "Default diff file state", Diffs, &["collapsed expanded collapse expand files pull request pr code tab"]),
        SearchItem::new("diff-layout", "Diff layout", Diffs, &["stacked split side by side unified inline view"]),
        SearchItem::new("proactive-panels", "Open pull requests and changes panels proactively", Diffs, &["proactive automatically open diff pull request pr right panel agent completion"]),
        SearchItem::new("pull-request-links-open-in-app", "Open pull requests in T3 Code", Diffs, &["pr commit links GitHub GitLab GitCafe Forgejo Bitbucket Azure DevOps browser external click default pull request view panel"]),
        SearchItem::new("agent-browser-access", "Agent browser access", Browser, &["allow disable enable open drive preview tools sessions project override"]),
        SearchItem::new("browser-profiles", "Browser profiles", Browser, &[""]).target("browser"),
        SearchItem::new("browser-default-profile", "Default browser profile", Browser, &[""]).target("browser-profiles"),
        SearchItem::new("browser-default-viewport", "Default browser viewport", Browser, &["preview size width height device desktop mobile rotate"]),
        SearchItem::new("browser-default-zoom", "Default browser zoom", Browser, &["preview page scale tabs percent"]),
        SearchItem::new("browser-default-appearance", "Default browser appearance", Browser, &["preview color scheme light dark system os"]),
        SearchItem::new("browser-recording-frame-rate", "Browser recording frame rate", Browser, &[""]),
        SearchItem::new("browser-recording-key-presses", "Show key presses in recordings", Browser, &["browser preview keyboard shortcuts keystrokes overlay capture"]),
        SearchItem::new("browser-recording-mouse-presses", "Show mouse presses in recordings", Browser, &["browser preview clicks buttons drag overlay capture"]),
        SearchItem::new("browser-link-target", "Open links in", Browser, &["links default browser in-app browser external open"]),
        SearchItem::new("browser-search-engine", "Search engine", Browser, &["address bar duckduckgo google bing brave kagi ecosia custom url"]),
        SearchItem::new("browser-auto-show-floating-preview", "Auto-show floating preview", Browser, &["agent opens browser device simulator pop into view hide"]),
        SearchItem::new("snap-shot-enabled", "SnapShots", SnapShot, &["window capture screenshot"]),
        SearchItem::new("snap-shot-accessibility", "Include app text", SnapShot, &["capture accessibility data text UI structure elements privacy omit agent context"]).target("snap-shot-enabled"),
        SearchItem::new("snap-shot-shortcut", "Capture shortcut", SnapShot, &[""]).target("snap-shot-enabled"),
        SearchItem::new("snap-shot-sound", "Capture sound", SnapShot, &[""]).target("snap-shot-enabled"),
        SearchItem::new("snap-shot-flash", "Capture flash", SnapShot, &[""]).target("snap-shot-enabled"),
        SearchItem::new("snap-shot-animations", "Capture animations", SnapShot, &[""]).target("snap-shot-enabled"),
        SearchItem::new("storage-worktree-keep-when", "Keep worktrees with local changes", Worktrees, &["any local files uncommitted changes edited tracked files ignored env build output cleanup"])
            .target("storage-worktrees"),
        SearchItem::new("storage-delete-now", "Delete now", Maintenance, &["storage cleanup run now results report removed kept failed"]),
        SearchItem::new("storage-worktrees", "Worktree cleanup", Worktrees, &["disk storage automatic delete deleted archived threads old inactive merged settled unchanged worktrees retention days project inherit off custom"]),
        SearchItem::new("storage-worktrees-location", "Worktree location", Worktrees, &["worktree location folder directory path drive external disk"]),
        SearchItem::new("storage-artifacts", "Artifacts and logs", Maintenance, &["disk storage delete old browser artifacts screenshots captures rotated logs cleanup retention"]),
        SearchItem::new("project-defaults", "Project defaults and overrides", Threads, &["model workspace environments projects inheritance checkout"]),
        SearchItem::new("project-overview", "Project overview", Projects, &["name icon emoji image checkout remove delete"]),
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
        SearchItem::new("provider-update-checks", "Provider update checks", Maintenance, &["installed cli versions newer available codex claude cursor grok fx opencode"]),
        SearchItem::new("continue-threads-after-server-update", "Continue threads after restarts", Maintenance, &["resume running active interrupted work restart reboot machine crash desktop update automatically"]),
        SearchItem::new("background-activity", "Background activity", Maintenance, &["balanced performance battery saver advanced git fetch provider health refresh host power monitor idle policy"]),
        SearchItem::new("new-threads", "New threads", Threads, &["default workspace mode draft local worktree"]),
        SearchItem::new("worktree-submodules", "Submodules", Worktrees, &["git submodule init recursive top-level none worktree t3.json"]),
        SearchItem::new("start-from-origin", "Start from remote", Worktrees, &["new worktrees start remote origin latest fetched branch local"]),
        SearchItem::new("add-project-starts-in", "Add project starts in", Threads, &["base directory folder browser path home"]),
        SearchItem::new("unpin-confirmation", "Unpin confirmation", General, &["ask before thread pinned section"]),
        SearchItem::new("archive-confirmation", "Archive confirmation", General, &["ask before thread second click inline action"]),
        SearchItem::new("delete-confirmation", "Delete confirmation", General, &["ask before thread chat history"]),
        SearchItem::new("quit-confirmation", "Quit shortcut", General, &["confirmation desktop app exit direct hold double click press twice"]),
        SearchItem::new("app-version", "Version", About, &["about check for updates download install upgrade release"]),
        SearchItem::new("telemetry-export", "OpenTelemetry export", Diagnostics, &["OTel OTLP telemetry export endpoints logs traces metrics Grafana"]),
        SearchItem::new("diagnostics", "Diagnostics", Maintenance, &["logs traces processes resource history failures spans cpu memory"]),
        SearchItem::new("legacy-plan-mode", "Plan mode (legacy)", Labs, &["build plan composer old"]),
        SearchItem::new("legacy-context-window-indicator", "Context window indicator (legacy)", Labs, &["composer meter usage tokens circle old"]),
        SearchItem::new("keybindings", "Keybindings", Keybindings, &["keyboard shortcuts hotkeys commands bindings json"]),
        SearchItem::new("providers", "Providers", Providers, &["agents cli codex claude cursor grok fx opencode antigravity google sign in sign out install subscription instances authentication api key models configuration binary path config directory codex_home shadow home endpoint arguments environment variables display name accent color custom favorite hidden auto compact native sessions import resume"]),
        SearchItem::new("provider-health-check-interval", "Health check interval", Providers, &["refresh availability versions auth state models background probes seconds off"]),
        SearchItem::new("agent-device-access", "Agents can use devices", Devices, &["allow simulator emulator ios android drive tools sessions"]).target("devices"),
        SearchItem::new("device-hub", "Use devices", Devices, &["device hub simulator emulator ios android install start version"]).target("devices"),
        SearchItem::new("automatic-pull", "Automatically pull", SourceControl, &["auto pull default branch current checkout fast forward upstream"]),
        SearchItem::new("remove-agent-credits-on-merge", "Remove agent credits when merging", SourceControl, &["pull request GitHub GitLab GitCafe Forgejo Bitbucket Azure DevOps squash co-authored-by attribution claude codex generated"]),
        SearchItem::new("pull-request-merge-method", "Default merge method", SourceControl, &["pull request merge squash rebase last selected"]),
        SearchItem::new("source-control", "Source control", SourceControl, &["version control git GitHub ghes enterprise GitLab GitCafe Forgejo gitea tea codeberg Bitbucket Azure DevOps hosting integrations credentials scan server environment"]),
        SearchItem::new("git-fetch-interval", "Git fetch interval", Maintenance, &["automatic remote branch refresh background credentials security keys seconds off"]).target("background-activity"),
        SearchItem::new("worktree-branch-naming", "Worktree branch naming", Worktrees, &["static semantic prefix custom prompt instructions feat fix refactor chore"]),
        SearchItem::new("source-control-writing-style", "Source control writing style", SourceControl, &["repository conventions conventional commits custom instructions change descriptions request titles"]),
        SearchItem::new("follow-change-request-templates", "Follow change request templates", SourceControl, &["repository pr pull request description structure"]),
        SearchItem::new("source-control-writer-model", "Source control writer model", SourceControl, &["override generated commit change request pr titles descriptions branch bookmark"]),
        SearchItem::new("review-guides", "Review guides (beta)", SourceControl, &["pull request pr guided review guide tour beta experimental"]),
        SearchItem::new("review-guide-model", "Review guide model", SourceControl, &["pull request pr guided review guide brief maintainer"]).target("review-guides"),
        SearchItem::new("review-guide-instructions", "Review guide instructions", SourceControl, &["pull request pr guided review guide preferences taste prompt custom"]).target("review-guides"),
        SearchItem::new("project-actions", "Actions", Projects, &["commands scripts setup run dev server checkout worktree t3.json import"]),
        SearchItem::new("connections-environment", "This machine", Access, &["connections server backend local remote access administrative permissions scope pairing links qr code authorized clients sessions revoke endpoint"]),
        SearchItem::new("remote-environments", "Environments", Connections, &["add pair backend host code ssh config agent tunnel saved t3 connect"]),
        SearchItem::new("secret-providers", "Secret providers", Secrets, &["1password vault credentials api key"]),
        SearchItem::new("secret-providers-add", "Add secret provider", Secrets, &["new 1password vault op cli account"]).target("secret-providers"),
        SearchItem::new("archive", "Archived threads", Archived, &["restore reopen deleted history projects"]),
    ];
    items.retain(|item| item.section.is_available(features));
    items
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
