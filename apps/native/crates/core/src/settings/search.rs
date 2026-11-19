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
    /// Voice input, transcription models, dictionary, and cleanup settings (`VoiceSettingsPanel.tsx`).
    Voice,
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
            Section::Voice => "Voice",
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
                | Section::Voice
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
    (None, &[Section::General, Section::Appearance, Section::Chat, Section::Diffs, Section::Keybindings, Section::Voice, Section::SnapShot]),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Environment,
    EnvironmentDefaults,
    ProjectDefaults,
    Project,
    Checkout,
    Connections,
}

/// Environment- and platform-dependent gates matching `SettingsSearchAvailability`
/// (`settingsSearch.ts`, `useAvailableSettingsSearchItems.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchAvailability {
    pub has_environment: bool,
    pub has_provider_settings_environment: bool,
    pub has_thread_auto_settlement: bool,
    pub has_peer_link_host: bool,
    pub has_fast_worktree_checkout_environment: bool,
    pub has_text_generators_environment: bool,
    pub has_multiple_environments: bool,
    pub is_macos: bool,
}

impl Default for SearchAvailability {
    fn default() -> Self {
        Self {
            has_environment: true,
            has_provider_settings_environment: true,
            has_thread_auto_settlement: true,
            has_peer_link_host: true,
            has_fast_worktree_checkout_environment: true,
            has_text_generators_environment: true,
            has_multiple_environments: true,
            is_macos: cfg!(target_os = "macos"),
        }
    }
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
    pub scope: Option<SearchScope>,
    pub mac_only: bool,
    pub environment_only: bool,
    pub provider_settings_only: bool,
    pub requires_thread_auto_settlement: bool,
    pub peer_links_only: bool,
    pub fast_worktree_checkout_only: bool,
    pub text_generators_only: bool,
    pub multi_environment_only: bool,
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
            scope: None,
            mac_only: false,
            environment_only: false,
            provider_settings_only: false,
            requires_thread_auto_settlement: false,
            peer_links_only: false,
            fast_worktree_checkout_only: false,
            text_generators_only: false,
            multi_environment_only: false,
        }
    }

    fn target(mut self, target: &str) -> SearchItem {
        self.target_id = Some(target.to_owned());
        self
    }

    fn scope(mut self, scope: SearchScope) -> SearchItem {
        self.scope = Some(scope);
        self
    }

    fn mac_only(mut self) -> SearchItem {
        self.mac_only = true;
        self
    }

    fn environment_only(mut self) -> SearchItem {
        self.environment_only = true;
        self
    }

    fn provider_settings_only(mut self) -> SearchItem {
        self.provider_settings_only = true;
        self
    }

    fn requires_thread_auto_settlement(mut self) -> SearchItem {
        self.requires_thread_auto_settlement = true;
        self
    }

    fn peer_links_only(mut self) -> SearchItem {
        self.peer_links_only = true;
        self
    }

    fn fast_worktree_checkout_only(mut self) -> SearchItem {
        self.fast_worktree_checkout_only = true;
        self
    }

    fn text_generators_only(mut self) -> SearchItem {
        self.text_generators_only = true;
        self
    }

    fn multi_environment_only(mut self) -> SearchItem {
        self.multi_environment_only = true;
        self
    }

    /// Where a jump lands: the target row, else the item's own row.
    pub fn anchor(&self) -> &str {
        self.target_id.as_deref().unwrap_or(&self.id)
    }

    pub fn is_available(&self, availability: SearchAvailability) -> bool {
        (!self.mac_only || availability.is_macos)
            && (!self.environment_only || availability.has_environment)
            && (!self.provider_settings_only || availability.has_provider_settings_environment)
            && (!self.requires_thread_auto_settlement || availability.has_thread_auto_settlement)
            && (!self.peer_links_only || availability.has_peer_link_host)
            && (!self.fast_worktree_checkout_only || availability.has_fast_worktree_checkout_environment)
            && (!self.text_generators_only || availability.has_text_generators_environment)
            && (!self.multi_environment_only || availability.has_multiple_environments)
    }
}

/// `SETTINGS_SEARCH_ITEMS` for the rows the native panels render, in the web's order.
pub fn items(features: Features) -> Vec<SearchItem> {
    use SearchScope::{Environment, EnvironmentDefaults, Project, ProjectDefaults};
    use Section::*;
    let mut items = vec![
        SearchItem::new("diff-color-scheme", "Diff colors", Diffs, &["red green blue orange additions deletions changes counts palette colorblind"]),
        SearchItem::new("hide-whitespace-changes", "Hide whitespace changes", Diffs, &["diff ignore spaces edits default"]),
        SearchItem::new("default-diff-file-state", "Default diff file state", Diffs, &["collapsed expanded collapse expand files pull request pr code tab"]),
        SearchItem::new("diff-layout", "Diff layout", Diffs, &["stacked split side by side unified inline view"]),
        SearchItem::new("proactive-panels", "Open pull requests and changes panels proactively", Diffs, &["proactive automatically open diff pull request pr right panel agent completion"]),
        SearchItem::new("pull-request-links-open-in-app", "Open pull requests in T3 Code", Diffs, &["pr commit links GitHub GitLab GitCafe Forgejo Bitbucket Azure DevOps browser external click default pull request view panel"]),
        SearchItem::new("agent-browser-access", "Agent browser access", Browser, &["allow disable enable open drive preview tools sessions project override"]).scope(ProjectDefaults),
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
            .target("storage-worktrees")
            .scope(ProjectDefaults),
        SearchItem::new("storage-delete-now", "Delete now", Maintenance, &["storage cleanup run now results report removed kept failed"]).scope(EnvironmentDefaults),
        SearchItem::new("storage-worktrees", "Worktree cleanup", Worktrees, &["disk storage automatic delete deleted archived threads old inactive merged settled unchanged worktrees retention days project inherit off custom"]).scope(ProjectDefaults),
        SearchItem::new("storage-worktrees-location", "Worktree location", Worktrees, &["worktree location folder directory path drive external disk"]).scope(EnvironmentDefaults),
        SearchItem::new("storage-artifacts", "Artifacts and logs", Maintenance, &["disk storage delete old browser artifacts screenshots captures rotated logs cleanup retention"]).scope(EnvironmentDefaults),
        SearchItem::new("project-defaults", "Project defaults and overrides", Threads, &["model workspace environments projects inheritance checkout"]).scope(ProjectDefaults),
        SearchItem::new("project-overview", "Project overview", Projects, &["name icon emoji image checkout remove delete"]).scope(Project),
        SearchItem::new("default-model", "Default model", Threads, &["new thread project provider reasoning effort"]).scope(ProjectDefaults),
        SearchItem::new("fallback-model", "Fallback model", Threads, &["fallback agent provider out of usage new threads"]).scope(EnvironmentDefaults).environment_only(),
        SearchItem::new("default-permissions", "Permissions", Threads, &["new thread default runtime mode supervised approvals auto accept edits full access"]).scope(ProjectDefaults),
        SearchItem::new("color-scheme", "Color scheme", Appearance, &["appearance light dark system mode"]).target("appearance"),
        SearchItem::new("theme", "Themes", Appearance, &["appearance colors palette custom import"]).target("appearance"),
        SearchItem::new("setting-appearance-contrast", "Contrast", Appearance, &["colors borders readability contrast"]),
        SearchItem::new("setting-glass-opacity", "Glass opacity", Appearance, &["menus dialogs composer transparency glass"]),
        SearchItem::new("chat-width", "Chat width", Appearance, &["wide full width column layout messages composer monitor"]),
        SearchItem::new(
            "thread-details-items",
            "Thread details",
            Appearance,
            &["customize show hide panel card environment editor scripts branch pull request git changes servers automations lineage usage limits 5 hour weekly quota tokens context window"],
        ),
        SearchItem::new(
            "effort-display",
            "Effort display",
            Appearance,
            &["reasoning effort thinking bars graph word label level composer"],
        ),
        SearchItem::new(
            "math-rendering",
            "Render math",
            Appearance,
            &["latex tex katex equations formulas markdown"],
        ),
        SearchItem::new("panel-animations", "Panel animations", Appearance, &[]),
        SearchItem::new(
            "environment-identification",
            "Environment identification",
            Appearance,
            &["dev nightly latest channel artwork pill label hide none"],
        )
        .target("appearance-interface"),
        SearchItem::new("prompt-font", "Prompt font", Appearance, &["typography composer input family size"]),
        SearchItem::new("code-font", "Code font", Appearance, &["typography monospace family size diffs previews"]),
        SearchItem::new("terminal-font", "Terminal font", Appearance, &["typography monospace output family size"]),
        SearchItem::new("separate-fonts", "Separate fonts", Appearance, &["advanced typography prompt code terminal"]),
        SearchItem::new("font-smoothing", "Font smoothing", Appearance, &["advanced typography grayscale antialiased macOS thin"]).mac_only(),
        SearchItem::new("word-wrap", "Word wrap", Appearance, &["code diffs file previews long lines typography"]),
        SearchItem::new("interface-font", "Interface font", Appearance, &["typography family size system sans"]),
        SearchItem::new("composer-context", "Composer context", Appearance, &[]),
        SearchItem::new("project-grouping", "Project grouping", General, &["combine matching repositories environments sidebar"]),
        SearchItem::new("project-order", "Project order", General, &["sort projects sidebar manual created recent"]),
        SearchItem::new("snooze-options", "Snooze options", Threads, &["snooze presets times later tonight tomorrow next week remind"]).scope(EnvironmentDefaults).environment_only(),
        SearchItem::new("snooze-limited-threads", "Snooze until limits reset", Threads, &["usage quota rate limit reset wake recover continue"]),
        SearchItem::new("auto-resume-limited-threads", "Resume after usage limits", Threads, &["usage quota rate limit reset recover continue"]),
        SearchItem::new("working-shelf", "Working section", General, &["hide fold running monitoring threads inbox sidebar shelf"]),
        SearchItem::new("boring-sidebar", "Boring sidebar (group by project)", Labs, &["beta folders project tree classic sidebar"]),
        SearchItem::new("boring-sidebar-multiline", "Two-line folder rows", Labs, &["beta boring sidebar folder rows multiline branch pull request details"]).target("boring-sidebar"),
        SearchItem::new("guest-links", "Guest links", Labs, &["beta share guest invite link teammate watch t3 connect tunnel"]).scope(Environment),
        SearchItem::new("linked-environments-beta", "Linked environments", Labs, &["beta link another machine vps peer agents work there delegate"]).scope(Environment),
        SearchItem::new("prompt-suggestions-beta", "Suggest next prompt", Labs, &["beta tab autocomplete predict next message claude codex ghost text"]).scope(Environment),
        SearchItem::new("simlock-hosts", "Simlock device hosts (alpha)", Labs, &["beta alpha simlock callstack simulator emulator ios android pool lease gateway"]).scope(Environment),
        SearchItem::new("worktree-fast-checkout", "Fast worktree checkout", Labs, &["beta fast worktree checkout clone apfs copy-on-write cache macos speed"]).scope(Environment).fast_worktree_checkout_only(),
        SearchItem::new("voice-transcription-destination", "Transcribe with", Voice, &["send microphone audio dictation destination speech voice stt transcription environment machine consent privacy cloud OpenAI Grok OpenRouter"]),
        SearchItem::new("voice-microphone", "Microphone", Voice, &["mic input audio device order priority preferred"]),
        SearchItem::new("dictation-shortcut-mode", "Dictation shortcut", Voice, &["speech voice keyboard hotkey hold toggle auto"]),
        SearchItem::new("speech-language", "Transcription language", Voice, &["speech voice translate english auto detect"]),
        SearchItem::new("speech-dictionary", "Dictionary", Voice, &["speech voice transcription vocabulary names glossary alias"]),
        SearchItem::new("speech-project-dictionary", "Project dictionary", Voice, &["speech voice transcription vocabulary shared dictionary team environment glossary"]).scope(ProjectDefaults),
        SearchItem::new("speech-post-processing-model", "Cleanup model", Voice, &["speech voice transcript cleanup provider model post-processing"]).scope(Environment),
        SearchItem::new("speech-correction-word", "Correction cue", Voice, &["speech voice cleanup sorry scratch that fix mistake"]),
        SearchItem::new("speech-silence-filtering", "Silence filtering", Voice, &["voice speech vad silero pauses noise"]),
        SearchItem::new("remove-filler-words", "Remove filler words", Voice, &["voice speech transcription hesitation um uh cleanup"]),
        SearchItem::new("speech-post-processing", "Clean up transcripts", Voice, &["speech transcription polish clean provider prompt post-processing"]),
        SearchItem::new("speech-models", "Transcription models", Voice, &["speech voice stt download whisper parakeet canary moonshine accuracy speed benchmark languages"]),
        SearchItem::new("speech-hardware", "Transcription hardware and model unloading", Voice, &["speech voice stt advanced gpu cpu acceleration memory unload idle"]),
        SearchItem::new("microphone-test", "Test microphone", Voice, &["test mic audio playback loopback record input level"]),
        SearchItem::new("transcription-test", "Test model", Voice, &["test voice speech model transcription recognition"]),
        SearchItem::new("auto-settle-inactive-threads", "Auto-settle inactive threads", Threads, &["sidebar inactivity days no activity automatically"]).scope(ProjectDefaults).requires_thread_auto_settlement(),
        SearchItem::new("compact-idle-threads", "Compact idle threads", Providers, &["claude context compaction idle cache tokens usage summary resume"]).scope(EnvironmentDefaults),
        SearchItem::new("auto-settle-merged-threads", "Auto-settle merged threads", Threads, &["pull request merge closed automatically sidebar"]).scope(ProjectDefaults).requires_thread_auto_settlement(),
        SearchItem::new("keep-threads-with-pull-requests", "Keep threads with a PR", Threads, &["pull request inactivity auto-settle merge close sidebar"])
            .target("auto-settle-inactive-threads")
            .scope(ProjectDefaults)
            .requires_thread_auto_settlement(),
        SearchItem::new("days-before-auto-settle", "Days of inactivity before auto-settle", Threads, &["thread timeout activity sidebar"])
            .target("auto-settle-inactive-threads")
            .scope(ProjectDefaults)
            .requires_thread_auto_settlement(),
        SearchItem::new("thread-notifications", "Thread notifications", General, &["notification sound alert completion input approval desktop"]),
        SearchItem::new("in-app-notifications", "In-app notifications", General, &["notification toast popup completion input approval failure"]),
        SearchItem::new(
            "language",
            "Language",
            General,
            &["locale translation english deutsch german español spanish français french português portuguese brasil brazilian chinese 中文 japanese 日本語 korean 한국어 russian русский sprache idioma langue 语言 言語 언어 язык"],
        ),
        SearchItem::new("time-format", "Time format", General, &["timestamp clock locale system browser os 12 hour 24 hour"]),
        SearchItem::new("response-streaming", "Response streaming", Threads, &["output token paragraph buffered wait turn legacy"]).scope(ProjectDefaults),
        SearchItem::new("skills-in-slash-menu", "Show skills in slash menu", Chat, &["command menu dollar $ slash /"]),
        SearchItem::new("composer-rich-text", "Rich text composer", Chat, &["composer rich text tiptap bold italic markdown styled wysiwyg"]),
        SearchItem::new("send-shortcut", "Send shortcut", Chat, &["enter return command ctrl multiline prompt new line composer"]),
        SearchItem::new("follow-up-behavior", "Follow-up behavior", Chat, &["queue steer running turn send default behavior composer"]),
        SearchItem::new("tool-call-visibility", "Tool calls in chat", Chat, &["activity work log tool calls show all hide successful hide all zen"]),
        SearchItem::new(
            "screen-reader-announcements",
            "Screen reader announcements",
            Chat,
            &["accessibility a11y screen reader nvda jaws voiceover orca narrator speech announce read aloud messages activity steps"],
        ),
        SearchItem::new(
            "screen-reader-window-title",
            "Screen reader window title",
            Chat,
            &["accessibility a11y screen reader nvda jaws voiceover orca narrator window title tab title alt tab taskbar thread status order working approval"],
        ),
        SearchItem::new("provider-update-checks", "Provider update checks", Maintenance, &["installed cli versions newer available codex claude cursor grok fx kiro opencode"]).scope(EnvironmentDefaults),
        SearchItem::new("continue-threads-after-server-update", "Continue threads after restarts", Maintenance, &["resume running active interrupted work restart reboot machine crash desktop update automatically"]).scope(ProjectDefaults),
        SearchItem::new("keep-awake-while-agents-work", "Keep awake while agents work", Maintenance, &["caffeinate sleep prevent stay awake power laptop lid inhibit"]).scope(EnvironmentDefaults),
        SearchItem::new("background-activity", "Background activity", Maintenance, &["balanced performance battery saver advanced git fetch provider health refresh host power monitor idle policy"]).scope(EnvironmentDefaults),
        SearchItem::new("new-threads", "New threads", Threads, &["default workspace mode draft local worktree"]).scope(ProjectDefaults),
        SearchItem::new("worktree-submodules", "Submodules", Worktrees, &["git submodule init recursive top-level none worktree t3.json"]).scope(ProjectDefaults),
        SearchItem::new("start-from-origin", "Start from remote", Worktrees, &["new worktrees start remote origin latest fetched branch local"]).scope(ProjectDefaults),
        SearchItem::new("add-project-starts-in", "Add project starts in", Threads, &["base directory folder browser path home"]).scope(EnvironmentDefaults),
        SearchItem::new("ghq-clone-layout", "Clone into host folders", Threads, &["ghq clone destination host owner repository root layout"]).scope(EnvironmentDefaults),
        SearchItem::new("unpin-confirmation", "Unpin confirmation", General, &["ask before thread pinned section"]),
        SearchItem::new("archive-confirmation", "Archive confirmation", General, &["ask before thread second click inline action"]),
        SearchItem::new("delete-confirmation", "Delete confirmation", General, &["ask before thread chat history"]),
        SearchItem::new("quit-confirmation", "Quit shortcut", General, &["confirmation desktop app exit direct hold double click press twice"]),
        SearchItem::new("text-generation-model", "Text generation model", Threads, &["generated thread titles source control content default provider"]).scope(ProjectDefaults),
        SearchItem::new("thread-title-instructions", "Thread title instructions", Threads, &["generated thread titles prompt custom style naming rename regenerate"]).scope(ProjectDefaults),
        SearchItem::new("app-version", "Version", About, &["about check for updates download install upgrade release"]),
        SearchItem::new("mobile-app", "Mobile app", About, &["phone tablet iPhone iPad iOS Android App Store Google Play QR"]),
        SearchItem::new(
            "welcome-page",
            "Welcome page",
            About,
            &["onboarding setup first run getting started agents import projects"],
        )
        .environment_only(),
        SearchItem::new("privacy-policy", "Privacy policy", About, &["telemetry analytics usage data tracking legal opt out"]),
        SearchItem::new("open-source-licenses", "Open source licenses", About, &["third party notices dependencies mit apache"]),
        SearchItem::new("telemetry-export", "OpenTelemetry export", Diagnostics, &["OTel OTLP telemetry export endpoints logs traces metrics Grafana"]).scope(Environment),
        SearchItem::new("diagnostics", "Diagnostics", Maintenance, &["logs traces processes resource history failures spans cpu memory"]),
        SearchItem::new("keybindings", "Keybindings", Keybindings, &["keyboard shortcuts hotkeys commands bindings json"]),
        SearchItem::new("providers", "Providers", Providers, &["agents cli codex claude cursor grok fx kiro opencode antigravity google sign in sign out install subscription instances authentication api key models configuration binary path config directory codex_home shadow home endpoint arguments environment variables display name accent color custom favorite hidden auto compact native sessions import resume"]),
        SearchItem::new("text-generators", "Text generators", Providers, &["model api api key openai openrouter anthropic ollama lm studio custom endpoint base url thread titles commit messages"]).provider_settings_only().text_generators_only(),
        SearchItem::new("provider-health-check-interval", "Health check interval", Providers, &["refresh availability versions auth state models background probes seconds off"]).provider_settings_only(),
        SearchItem::new("scheduled-tasks", "Scheduled tasks", ScheduledTasks, &["automations cron schedule recurring webhook"]),
        SearchItem::new("device-sources", "Device sources", Devices, &["device hosts ssh remote mac mini identity key connection eas expo cloud simlock pool gateway local simulators"]),
        SearchItem::new("agent-device-access", "Agents can use devices", Devices, &["allow simulator emulator ios android drive tools sessions"]).target("devices"),
        SearchItem::new("device-hub", "Use devices", Devices, &["device hub simulator emulator ios android install start version"]).target("devices"),
        SearchItem::new("device-platform-support", "Local simulators", Devices, &["simulator support xcode android studio sdk avd runtime"]).target("device-sources"),
        SearchItem::new("automatic-pull", "Automatically pull", SourceControl, &["auto pull default branch current checkout fast forward upstream"]).scope(ProjectDefaults),
        SearchItem::new("remove-agent-credits-on-merge", "Remove agent credits when merging", SourceControl, &["pull request GitHub GitLab GitCafe Forgejo Bitbucket Azure DevOps squash co-authored-by attribution claude codex generated"]).scope(ProjectDefaults),
        SearchItem::new("pull-request-merge-method", "Default merge method", SourceControl, &["pull request merge squash rebase last selected"]).scope(ProjectDefaults),
        SearchItem::new("source-control", "Source control", SourceControl, &["version control git GitHub ghes enterprise GitLab GitCafe Forgejo gitea tea codeberg Bitbucket Azure DevOps hosting integrations credentials scan server environment"]).scope(EnvironmentDefaults),
        SearchItem::new("git-fetch-interval", "Git fetch interval", Maintenance, &["automatic remote branch refresh background credentials security keys seconds off"]).target("background-activity").scope(EnvironmentDefaults).environment_only(),
        SearchItem::new("worktree-branch-naming", "Worktree branch naming", Worktrees, &["static semantic prefix custom prompt instructions feat fix refactor chore"]).scope(ProjectDefaults).environment_only(),
        SearchItem::new("source-control-writing-style", "Commit and pull request writing", SourceControl, &["repository conventions conventional commits custom instructions commit messages change pr pull request merge request titles descriptions body style prompt write my own"]).environment_only(),
        SearchItem::new("follow-change-request-templates", "Follow pull request templates", SourceControl, &["repository pr pull request merge request change request description structure"]).environment_only(),
        SearchItem::new("source-control-writer-model", "Source control writer model", SourceControl, &["override generated commit change request pr titles descriptions branch bookmark"]).scope(ProjectDefaults).environment_only(),
        SearchItem::new("review-guides", "Review guides (beta)", SourceControl, &["pull request pr guided review guide tour beta experimental"]).environment_only(),
        SearchItem::new("review-guide-model", "Review guide model", SourceControl, &["pull request pr guided review guide brief maintainer"]).target("review-guides").scope(ProjectDefaults).environment_only(),
        SearchItem::new("review-guide-instructions", "Review guide instructions", SourceControl, &["pull request pr guided review guide preferences taste prompt custom"]).target("review-guides").scope(ProjectDefaults).environment_only(),
        SearchItem::new("project-actions", "Actions", Projects, &["commands scripts setup run dev server checkout worktree t3.json import"]).scope(Project),
        SearchItem::new("connections-environment", "This machine", Access, &["connections server backend local remote access administrative permissions scope pairing links qr code authorized clients sessions revoke endpoint"]).scope(SearchScope::Connections),
        SearchItem::new("linked-environments", "Linked environments", Connections, &["link peer another machine vps agents work there handoff pairing code"]).scope(SearchScope::Connections).peer_links_only(),
        SearchItem::new("remote-environments", "Environments", Connections, &["add pair backend host code ssh config agent tunnel saved t3 connect"]).scope(SearchScope::Connections),
        SearchItem::new("load-balancing", "Load balancing", Connections, &["automatic machine environment resources cpu memory capacity preference weight shared projects"]).scope(SearchScope::Connections).multi_environment_only(),
        SearchItem::new("pull-request-routing", "Pull request sharing", Connections, &["pull request routing trusted environments shared credentials permissions read actions source control GitHub GitLab GitCafe Forgejo Bitbucket Azure DevOps"]).scope(SearchScope::Connections).multi_environment_only(),
        SearchItem::new("secret-providers", "Secret providers", Secrets, &["1password vault credentials api key"]),
        SearchItem::new("secret-providers-add", "Add secret provider", Secrets, &["new 1password vault op cli account"]).target("secret-providers"),
        SearchItem::new("archive", "Archived threads", Archived, &["restore reopen deleted history projects"]).scope(ProjectDefaults),
    ];
    items.retain(|item| item.section.is_available(features));
    items
}

/// `filterAvailableSettingsSearchItems` + `settingsForThisPlatform` (`settingsSearch.ts`).
pub fn available_items(features: Features, availability: SearchAvailability) -> Vec<SearchItem> {
    items(features)
        .into_iter()
        .filter(|item| item.is_available(availability))
        .collect()
}

/// `isSettingsSearchScopeAvailable` (`settingsSearch.ts`).
pub fn is_search_scope_available(
    required: Option<SearchScope>,
    scope_kind: super::ResolvedScopeKind,
) -> bool {
    match required {
        None | Some(SearchScope::Connections) => true,
        Some(SearchScope::Environment) => scope_kind == super::ResolvedScopeKind::Environment,
        Some(SearchScope::Checkout) => scope_kind == super::ResolvedScopeKind::Checkout,
        Some(SearchScope::Project) => {
            matches!(scope_kind, super::ResolvedScopeKind::Project | super::ResolvedScopeKind::Checkout)
        }
        Some(SearchScope::EnvironmentDefaults) => {
            matches!(scope_kind, super::ResolvedScopeKind::Environment | super::ResolvedScopeKind::All)
        }
        Some(SearchScope::ProjectDefaults) => matches!(
            scope_kind,
            super::ResolvedScopeKind::Environment
                | super::ResolvedScopeKind::All
                | super::ResolvedScopeKind::Project
                | super::ResolvedScopeKind::Checkout
        ),
    }
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
        scope: None,
        mac_only: false,
        environment_only: false,
        provider_settings_only: false,
        requires_thread_auto_settlement: false,
        peer_links_only: false,
        fast_worktree_checkout_only: false,
        text_generators_only: false,
        multi_environment_only: false,
    }
}

/// `normalizeSearchText`: lowercase, combining marks dropped, whitespace collapsed.
pub fn normalize(value: &str) -> String {
    let stripped: String =
        value.chars().filter(|c| !('\u{0300}'..='\u{036f}').contains(c)).flat_map(char::to_lowercase).collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `searchSettings`: every query token must appear in the title, the section label, or a term
/// (matching both English and active-locale labels). Primary results first, then by rank
/// (exact title, title prefix, title phrase, title tokens, any phrase), then list order.
pub fn search<'a>(query: &str, items: &'a [SearchItem]) -> Vec<&'a SearchItem> {
    search_with_locale(query, items, crate::i18n::DEFAULT_LOCALE)
}

/// `searchSettings` with localized title/section matching (`settingsSearch.ts:1547-1591`).
pub fn search_with_locale<'a>(query: &str, items: &'a [SearchItem], locale: &str) -> Vec<&'a SearchItem> {
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
            let translated_title = normalize(crate::i18n::translate(locale, &item.title));
            let translated_section = normalize(crate::i18n::translate(locale, item.section.label()));
            let mut fields = vec![
                title.clone(),
                normalize(item.section.label()),
                translated_title,
                translated_section,
            ];
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

#[cfg(test)]
mod search_parity_tests {
    use super::*;

    #[test]
    fn voice_kiro_text_generators_and_environments_items_are_searchable_and_gated() {
        let all = available_items(Features::default(), SearchAvailability::default());
        for expected_id in [
            "voice-transcription-destination",
            "voice-microphone",
            "dictation-shortcut-mode",
            "speech-language",
            "speech-dictionary",
            "speech-project-dictionary",
            "speech-post-processing-model",
            "speech-correction-word",
            "speech-silence-filtering",
            "remove-filler-words",
            "speech-post-processing",
            "speech-models",
            "speech-hardware",
            "microphone-test",
            "transcription-test",
            "text-generators",
            "linked-environments",
            "load-balancing",
            "pull-request-routing",
            "ghq-clone-layout",
            "text-generation-model",
            "thread-title-instructions",
        ] {
            assert!(
                all.iter().any(|item| item.id == expected_id),
                "missing search item {expected_id}"
            );
        }

        // Kiro is searchable under Providers and Maintenance.
        let kiro_hits = search("kiro", &all);
        assert!(kiro_hits.iter().any(|item| item.id == "providers"));

        // Text generators & peer links & load balancing respect availability flags.
        let restricted = available_items(
            Features::default(),
            SearchAvailability {
                has_environment: false,
                has_provider_settings_environment: false,
                has_thread_auto_settlement: false,
                has_peer_link_host: false,
                has_fast_worktree_checkout_environment: false,
                has_text_generators_environment: false,
                has_multiple_environments: false,
                is_macos: false,
            },
        );
        for hidden in [
            "text-generators",
            "linked-environments",
            "load-balancing",
            "pull-request-routing",
            "font-smoothing",
            "worktree-fast-checkout",
            "auto-settle-inactive-threads",
        ] {
            assert!(
                !restricted.iter().any(|item| item.id == hidden),
                "item {hidden} should be gated off"
            );
        }

        assert!(is_search_scope_available(Some(SearchScope::Project), super::super::ResolvedScopeKind::Project));
        assert!(!is_search_scope_available(Some(SearchScope::Project), super::super::ResolvedScopeKind::All));
        assert!(is_search_scope_available(Some(SearchScope::ProjectDefaults), super::super::ResolvedScopeKind::All));

        // Language and Screen reader items are searchable in English and active locales.
        assert!(all.iter().any(|item| item.id == "language"));
        assert!(all.iter().any(|item| item.id == "screen-reader-announcements"));
        assert!(all.iter().any(|item| item.id == "screen-reader-window-title"));

        // Localized search matches both translated and English titles/sections.
        let de_section_hits = search_with_locale("Allgemein", &all, "de");
        assert!(de_section_hits.iter().any(|item| item.id == "language"));
        let de_en_hits = search_with_locale("Time format", &all, "de");
        assert!(de_en_hits.iter().any(|item| item.id == "time-format"));
        let ja_hits = search_with_locale("時刻の形式", &all, "ja");
        assert!(ja_hits.iter().any(|item| item.id == "time-format"));
        let zh_hits = search_with_locale("语言", &all, "zh");
        assert!(zh_hits.iter().any(|item| item.id == "language"));

        // Appearance Interface rows + Render math + About Welcome page are searchable and gated.
        for expected_id in [
            "thread-details-items",
            "effort-display",
            "math-rendering",
            "panel-animations",
            "environment-identification",
            "composer-context",
            "welcome-page",
        ] {
            assert!(
                all.iter().any(|item| item.id == expected_id),
                "missing appearance/about search item {expected_id}"
            );
        }
        assert!(search("katex", &all).iter().any(|item| item.id == "math-rendering"));
        assert!(search("bars", &all).iter().any(|item| item.id == "effort-display"));
        assert!(search("usage limits", &all).iter().any(|item| item.id == "thread-details-items"));
        assert!(search("panel animations", &all).iter().any(|item| item.id == "panel-animations"));
        assert!(search("composer context", &all).iter().any(|item| item.id == "composer-context"));
        assert!(search("onboarding", &all).iter().any(|item| item.id == "welcome-page"));
        let env_id_item = all
            .iter()
            .find(|item| item.id == "environment-identification")
            .expect("environment-identification item");
        assert_eq!(env_id_item.anchor(), "appearance-interface");
        assert!(
            !restricted.iter().any(|item| item.id == "welcome-page"),
            "welcome-page should be gated when no environment is connected"
        );
    }
}
