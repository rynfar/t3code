//! Per-device settings the web keeps in local storage: `ClientSettingsSchema` in
//! `contracts/src/settings.ts`, plus the theme preferences `web/src/hooks/useTheme.ts` stores
//! under `t3code:theme` and `t3code:theme-appearance-mode`. The file keeps every key it does not
//! model, so a newer build's settings survive an older one, and an invalid value reads as the
//! contract's default without dropping the rest. Edits are saved as patches over the file as it
//! is at write time, never as this instance's whole copy.

pub mod typography;

use typography::{FontRole, ADVANCED_KEY};
use serde_json::{Map, Value};

use crate::composer::ClientSettings;
pub use crate::announcements::ScreenReaderAnnouncements;
pub use crate::notifications::NotificationMode;

macro_rules! literal_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal : $label:literal),+ $(,)? } default $default:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire),+
                }
            }

            pub fn parse(value: &str) -> Option<$name> {
                match value {
                    $($wire => Some($name::$variant),)+
                    _ => None,
                }
            }

            pub fn label(self) -> &'static str {
                match self {
                    $($name::$variant => $label),+
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default
            }
        }
    };
}

literal_enum!(
    DiffColorScheme {
        RedGreen => "red-green": "Red & green (default)",
        BlueOrange => "blue-orange": "Blue & orange",
    } default RedGreen
);

literal_enum!(
    ToolCallVisibility {
        ShowAll => "showAll": "Show all",
        HideSuccessful => "hideSuccessful": "Hide successful",
        HideAll => "hideAll": "Hide all",
        Zen => "zen": "Zen",
    } default ShowAll
);

literal_enum!(
    BrowserLinkTarget {
        System => "system": "Your default browser",
        App => "app": "T3 Code",
    } default System
);

literal_enum!(
    BrowserSearchEngine {
        DuckDuckGo => "duckduckgo": "DuckDuckGo",
        Google => "google": "Google",
        Bing => "bing": "Bing",
        Brave => "brave": "Brave Search",
        Kagi => "kagi": "Kagi",
        Ecosia => "ecosia": "Ecosia",
        Custom => "custom": "Custom",
    } default DuckDuckGo
);

literal_enum!(
    /// `SidebarProjectGroupingMode`. The switch shows on for both repository modes.
    ProjectGroupingMode {
        Repository => "repository": "Repository",
        RepositoryPath => "repository_path": "Repository path",
        Separate => "separate": "Separate",
    } default Repository
);

/// `PROJECT_GROUPING_MODE_LABELS` from `web/src/sidebarProjectGrouping.ts`.
pub const PROJECT_GROUPING_MODE_LABELS: &[(ProjectGroupingMode, &str)] = &[
    (ProjectGroupingMode::Repository, "Group by repository"),
    (ProjectGroupingMode::RepositoryPath, "Group by repository path"),
    (ProjectGroupingMode::Separate, "Keep separate"),
];

pub fn project_grouping_label(mode: ProjectGroupingMode) -> &'static str {
    PROJECT_GROUPING_MODE_LABELS.iter().find(|(value, _)| *value == mode).map(|(_, label)| *label).unwrap_or_default()
}

literal_enum!(
    /// `SidebarProjectSortOrder` with `SIDEBAR_PROJECT_SORT_ORDER_LABELS`.
    ProjectSortOrder {
        UpdatedAt => "updated_at": "Last user message",
        CreatedAt => "created_at": "Created at",
        Manual => "manual": "Manual",
    } default UpdatedAt
);

literal_enum!(
    /// `TimestampFormat` with `TIMESTAMP_FORMAT_LABELS`.
    TimestampFormat {
        Locale => "locale": "System default",
        TwelveHour => "12-hour": "12-hour",
        TwentyFourHour => "24-hour": "24-hour",
    } default Locale
);

literal_enum!(
    /// `QuitConfirmationMode` with `QUIT_CONFIRMATION_MODE_LABELS`.
    QuitConfirmationMode {
        Direct => "direct": "Direct",
        Hold => "hold": "Hold",
        DoubleClick => "double-click": "Double press",
    } default Hold
);

literal_enum!(
    /// `ChatWidth` with `CHAT_WIDTH_LABELS`.
    ChatWidth {
        Comfortable => "comfortable": "Comfortable",
        Wide => "wide": "Wide",
        Full => "full": "Full",
    } default Comfortable
);

literal_enum!(
    /// `EffortDisplay` (`packages/contracts/src/settings.ts`) with `EFFORT_DISPLAY_LABELS`
    /// (`AppearanceSettingsPanel.tsx`).
    EffortDisplay {
        Label => "label": "Word",
        Bars => "bars": "Bars",
    } default Label
);

literal_enum!(
    /// `EnvironmentIdentificationMode` (`packages/contracts/src/settings.ts`) with
    /// `ENVIRONMENT_IDENTIFICATION_LABELS` (`AppearanceSettingsPanel.tsx`).
    EnvironmentIdentificationMode {
        Artwork => "artwork": "Artwork",
        NightlyArtwork => "nightly-artwork": "Nightly artwork",
        DevArtwork => "dev-artwork": "Dev artwork",
        Pill => "pill": "Version pill",
        None => "none": "None",
    } default Artwork
);

pub const MIN_PANEL_ANIMATION_DURATION_MS: u32 = 0;
pub const MAX_PANEL_ANIMATION_DURATION_MS: u32 = 400;
pub const DEFAULT_PANEL_ANIMATION_DURATION_MS: u32 = 0;

/// `SidebarStageBackdropVariant` (`SidebarStageBackdrop.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageBackdropVariant {
    Dev,
    Nightly,
}

impl StageBackdropVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            StageBackdropVariant::Dev => "dev",
            StageBackdropVariant::Nightly => "nightly",
        }
    }
}

/// Matches `NIGHTLY_SERVER_VERSION_PATTERN = /^[^-+]+-(?:nightly|preview)\.\d{8}\.\d+$/`
/// (`apps/web/src/branding.logic.ts`).
pub fn is_nightly_server_version(version: &str) -> bool {
    let Some((prefix, rest)) = version.split_once('-') else {
        return false;
    };
    if prefix.is_empty() || prefix.contains('+') {
        return false;
    }
    let Some(suffix) = rest.strip_prefix("nightly.").or_else(|| rest.strip_prefix("preview.")) else {
        return false;
    };
    let Some((date, build)) = suffix.split_once('.') else {
        return false;
    };
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && !build.is_empty()
        && build.bytes().all(|b| b.is_ascii_digit())
}

/// `resolveServerBackedAppStageLabel` (`apps/web/src/branding.logic.ts`).
pub fn resolve_server_backed_stage_label<'a>(
    primary_server_version: Option<&str>,
    fallback_stage_label: &'a str,
) -> &'a str {
    if primary_server_version.is_some_and(is_nightly_server_version) {
        "Nightly"
    } else {
        fallback_stage_label
    }
}

/// `resolveSidebarStageBackdropVariant` (`apps/web/src/components/SidebarStageBackdrop.tsx`).
pub fn resolve_sidebar_stage_backdrop_variant(
    stage_label: &str,
    mode: EnvironmentIdentificationMode,
) -> Option<StageBackdropVariant> {
    match mode {
        EnvironmentIdentificationMode::NightlyArtwork => Some(StageBackdropVariant::Nightly),
        EnvironmentIdentificationMode::DevArtwork => Some(StageBackdropVariant::Dev),
        EnvironmentIdentificationMode::Artwork => match stage_label.trim().to_ascii_lowercase().as_str() {
            "nightly" => Some(StageBackdropVariant::Nightly),
            "dev" => Some(StageBackdropVariant::Dev),
            _ => None,
        },
        EnvironmentIdentificationMode::Pill | EnvironmentIdentificationMode::None => None,
    }
}

/// `resolveEnvironmentIdentificationPillLabel` (`apps/web/src/components/SidebarStageBackdrop.tsx`).
pub fn resolve_environment_identification_pill_label(stage_label: &str) -> Option<&'static str> {
    match stage_label.trim().to_ascii_lowercase().as_str() {
        "dev" => Some("Dev"),
        "nightly" => Some("Nightly"),
        _ => None,
    }
}

/// `resolveEnvironmentIdentificationMode` (`apps/web/src/hooks/useSettings.ts`): custom or
/// environment palette themes cannot show sidebar artwork and fall back to the version pill.
pub fn resolve_effective_environment_identification_mode(
    mode: EnvironmentIdentificationMode,
    palette_theme_active: bool,
    palette_theme_allows_artwork: bool,
) -> EnvironmentIdentificationMode {
    let is_artwork = !matches!(mode, EnvironmentIdentificationMode::Pill | EnvironmentIdentificationMode::None);
    if palette_theme_active && !palette_theme_allows_artwork && is_artwork {
        EnvironmentIdentificationMode::Pill
    } else {
        mode
    }
}

/// `environmentIdentificationOptions` (`apps/web/src/components/settings/AppearanceSettingsPanel.tsx`).
pub fn environment_identification_options(
    stage_variant: Option<StageBackdropVariant>,
) -> &'static [(EnvironmentIdentificationMode, &'static str)] {
    match stage_variant {
        Some(StageBackdropVariant::Dev) => &[
            (EnvironmentIdentificationMode::Artwork, "Dev artwork"),
            (EnvironmentIdentificationMode::NightlyArtwork, "Nightly artwork"),
            (EnvironmentIdentificationMode::Pill, "Version pill"),
            (EnvironmentIdentificationMode::None, "None"),
        ],
        Some(StageBackdropVariant::Nightly) => &[
            (EnvironmentIdentificationMode::Artwork, "Nightly artwork"),
            (EnvironmentIdentificationMode::DevArtwork, "Dev artwork"),
            (EnvironmentIdentificationMode::Pill, "Version pill"),
            (EnvironmentIdentificationMode::None, "None"),
        ],
        None => &[
            (EnvironmentIdentificationMode::Artwork, "None"),
            (EnvironmentIdentificationMode::NightlyArtwork, "Nightly artwork"),
            (EnvironmentIdentificationMode::DevArtwork, "Dev artwork"),
        ],
    }
}

/// `shouldShowComposerContextStrip` (`apps/web/src/components/BranchToolbar.logic.ts`).
pub fn should_show_composer_context_strip(
    is_draft_hero_state: bool,
    persist_in_active_threads: bool,
    has_active_project: bool,
    is_git_repo: bool,
    show_environment_indicator: bool,
) -> bool {
    has_active_project
        && (is_draft_hero_state
            || (persist_in_active_threads && (is_git_repo || show_environment_indicator)))
}

literal_enum!(
    /// `ThemePreferenceMode`: which half of a theme paints, or the OS's choice.
    AppearanceMode {
        System => "system": "System",
        Light => "light": "Light",
        Dark => "dark": "Dark",
    } default System
);

/// Every `NotificationMode` in the web's select order.
pub const NOTIFICATION_MODES: [NotificationMode; 4] =
    [NotificationMode::Off, NotificationMode::Notifications, NotificationMode::Sound, NotificationMode::NotificationsAndSound];

/// Every `ScreenReaderAnnouncements` mode in the web's select order (`SettingsPanels.tsx`).
pub const SCREEN_READER_ANNOUNCEMENTS_MODES: [ScreenReaderAnnouncements; 3] = [
    ScreenReaderAnnouncements::Status,
    ScreenReaderAnnouncements::Messages,
    ScreenReaderAnnouncements::Everything,
];

/// Wire value for `ScreenReaderAnnouncements`.
pub fn screen_reader_announcements_value(mode: ScreenReaderAnnouncements) -> &'static str {
    match mode {
        ScreenReaderAnnouncements::Status => "status",
        ScreenReaderAnnouncements::Messages => "messages",
        ScreenReaderAnnouncements::Everything => "everything",
    }
}

/// UI label for `ScreenReaderAnnouncements` (`SCREEN_READER_ANNOUNCEMENTS_LABELS`).
pub fn screen_reader_announcements_label(mode: ScreenReaderAnnouncements) -> &'static str {
    match mode {
        ScreenReaderAnnouncements::Status => "Status only",
        ScreenReaderAnnouncements::Messages => "Status and messages",
        ScreenReaderAnnouncements::Everything => "Everything",
    }
}

/// The wire value of a notification mode.
pub fn notification_mode_value(mode: NotificationMode) -> &'static str {
    match mode {
        NotificationMode::Off => "off",
        NotificationMode::Notifications => "notifications",
        NotificationMode::Sound => "sound",
        NotificationMode::NotificationsAndSound => "notifications-and-sound",
    }
}

pub const MIN_INTERFACE_FONT_SIZE: u32 = 12;
pub const MAX_INTERFACE_FONT_SIZE: u32 = 20;
pub const DEFAULT_INTERFACE_FONT_SIZE: u32 = 16;

pub const THEME_KEY: &str = "t3code:theme";
pub const THEME_APPEARANCE_MODE_KEY: &str = "t3code:theme-appearance-mode";

/// The stored theme: the default T3 Code theme in one of its modes, or a named theme by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemePreference {
    Default(AppearanceMode),
    Named(String),
}

impl ThemePreference {
    fn parse(value: &str) -> ThemePreference {
        match AppearanceMode::parse(value) {
            Some(mode) => ThemePreference::Default(mode),
            None => ThemePreference::Named(value.to_owned()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            ThemePreference::Default(mode) => mode.as_str(),
            ThemePreference::Named(id) => id,
        }
    }

    pub fn named_id(&self) -> Option<&str> {
        match self {
            ThemePreference::Named(id) => Some(id),
            ThemePreference::Default(_) => None,
        }
    }
}

/// One edit to the file, named by its `ClientSettingsSchema` key.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientSettingChange {
    DiffLayout(crate::diff::Layout),
    DiffIgnoreWhitespace(bool),
    DiffFilesCollapsed(bool),
    DiffColorScheme(DiffColorScheme),
    BrowserSearchEngine(BrowserSearchEngine),
    BrowserLinkTarget(BrowserLinkTarget),
    PullRequestLinksOpenInApp(bool),
    BrowserCustomSearchUrl(String),
    NotificationMode(NotificationMode),
    InAppNotifications(bool),
    ProjectGrouping(ProjectGroupingMode),
    ProjectGroupingOverride { key: String, mode: Option<ProjectGroupingMode> },
    ProjectSortOrder(ProjectSortOrder),
    WorkingShelf(bool),
    BoringSidebarEnabled(bool),
    BoringSidebarMultilineRows(bool),
    ConfirmThreadUnpin(bool),
    ConfirmThreadArchive(bool),
    ConfirmThreadDelete(bool),
    ConfirmQuit(QuitConfirmationMode),
    TimestampFormat(TimestampFormat),
    ChatWidth(ChatWidth),
    EffortDisplay(EffortDisplay),
    PanelAnimationDurationMs(u32),
    EnvironmentIdentificationMode(EnvironmentIdentificationMode),
    PersistComposerContextStrip(bool),
    ThreadDetailsItem { id: String, visible: bool },
    ResetThreadDetailsItems,
    ToolCallVisibility(ToolCallVisibility),
    InterfaceFontSize(u32),
    FontSize(FontRole, u32),
    FontFamily(FontRole, String),
    ResetFont(FontRole),
    SeparateFonts(bool),
    WordWrap(bool),
    MathRenderingEnabled(bool),
    ComposerRichText(bool),
    SendShortcut(crate::composer::SendShortcut),
    FollowUpBehavior(crate::composer::FollowUpBehavior),
    ShowSkillsInSlashMenu(bool),
    /// `setTheme`: replaces the theme and keeps the current appearance mode.
    Theme(ThemePreference),
    AppearanceMode(AppearanceMode),
    AppearanceContrast(u32),
    GlassOpacity(u32),
    ThemeLibrary(Value),
    ThemePatch(Map<String,Value>),
    ThemeHalves(std::collections::BTreeMap<String, String>),
    /// The last repository mode the grouping switch turned off, restored when it turns back on.
    LastEnabledProjectGrouping(ProjectGroupingMode),
    /// `completeOnboarding`: when the welcome wizard finished, as an ISO timestamp.
    OnboardingCompletedAt(String),
    VoiceMicrophones(Vec<crate::voice_input::VoiceMicrophoneEntry>),
    VoiceShortcutMode(crate::voice_input::VoiceShortcutMode),
    SpeechLanguage(String),
    SpeechTranslateToEnglish(bool),
    SpeechSilenceFiltering(bool),
    SpeechRemoveFillerWords(bool),
    SpeechCustomWords(Vec<crate::voice_input::SpeechCustomWordEntry>),
    SpeechPostProcessingPrompt(crate::voice_input::SpeechPostProcessingPromptSettings),
    SpeechCorrectionWord(String),
    Language(String),
    ProactivePanelsEnabled(bool),
    ScreenReaderAnnouncements(ScreenReaderAnnouncements),
    ScreenReaderWindowTitle(bool),
}

impl ClientSettingChange {
    fn entry(&self) -> (&'static str, Value) {
        use crate::composer::{FollowUpBehavior, SendShortcut};
        let text = |value: &str| Value::String(value.to_owned());
        match self {
            ClientSettingChange::FontSize(role, size) => (role.size_key(), Value::from(role.clamp(*size as f64))),
            ClientSettingChange::FontFamily(role, family) => (role.family_key(), text(&family.trim().chars().take(200).collect::<String>())),
            ClientSettingChange::ResetFont(role) => (role.family_key(), text("")),
            ClientSettingChange::SeparateFonts(on) => (ADVANCED_KEY, Value::Bool(*on)),
            ClientSettingChange::WordWrap(on) => ("wordWrap", Value::Bool(*on)),
            ClientSettingChange::MathRenderingEnabled(on) => ("mathRenderingEnabled", Value::Bool(*on)),
            ClientSettingChange::DiffLayout(layout) => ("diffLayout", serde_json::json!(layout)),
            ClientSettingChange::DiffIgnoreWhitespace(on) => {
                ("diffIgnoreWhitespace", Value::Bool(*on))
            }
            ClientSettingChange::DiffFilesCollapsed(on) => ("diffFilesCollapsed", Value::Bool(*on)),
            ClientSettingChange::DiffColorScheme(scheme) => {
                ("diffColorScheme", text(scheme.as_str()))
            }
            ClientSettingChange::BrowserLinkTarget(target) => ("browserLinkTarget", text(target.as_str())),
            ClientSettingChange::PullRequestLinksOpenInApp(on) => ("pullRequestLinksOpenInApp", Value::Bool(*on)),
            ClientSettingChange::BrowserSearchEngine(engine) => {
                ("browserSearchEngine", text(engine.as_str()))
            }
            ClientSettingChange::BrowserCustomSearchUrl(url) => (
                "browserCustomSearchUrl",
                text(&url.trim().chars().take(2048).collect::<String>()),
            ),
            ClientSettingChange::NotificationMode(mode) => ("notificationMode", text(notification_mode_value(*mode))),
            ClientSettingChange::InAppNotifications(on) => ("inAppNotificationsEnabled", Value::Bool(*on)),
            ClientSettingChange::ProjectGroupingOverride { .. } => unreachable!("grouping overrides apply to the current record"),
            ClientSettingChange::ProjectGrouping(mode) => ("sidebarProjectGroupingMode", text(mode.as_str())),
            ClientSettingChange::ProjectSortOrder(order) => ("sidebarProjectSortOrder", text(order.as_str())),
            ClientSettingChange::WorkingShelf(on) => ("sidebarWorkingShelfEnabled", Value::Bool(*on)),
            ClientSettingChange::BoringSidebarEnabled(on) => ("boringSidebarEnabled", Value::Bool(*on)),
            ClientSettingChange::BoringSidebarMultilineRows(on) => ("boringSidebarMultilineRows", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadUnpin(on) => ("confirmThreadUnpin", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadArchive(on) => ("confirmThreadArchive", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadDelete(on) => ("confirmThreadDelete", Value::Bool(*on)),
            ClientSettingChange::ConfirmQuit(mode) => ("confirmQuit", text(mode.as_str())),
            ClientSettingChange::TimestampFormat(format) => ("timestampFormat", text(format.as_str())),
            ClientSettingChange::ChatWidth(width) => ("chatWidth", text(width.as_str())),
            ClientSettingChange::EffortDisplay(display) => ("effortDisplay", text(display.as_str())),
            ClientSettingChange::PanelAnimationDurationMs(ms) => (
                "panelAnimationDurationMs",
                Value::from((*ms).clamp(MIN_PANEL_ANIMATION_DURATION_MS, MAX_PANEL_ANIMATION_DURATION_MS)),
            ),
            ClientSettingChange::EnvironmentIdentificationMode(mode) => {
                ("environmentIdentificationMode", text(mode.as_str()))
            }
            ClientSettingChange::PersistComposerContextStrip(on) => {
                ("persistComposerContextStrip", Value::Bool(*on))
            }
            ClientSettingChange::ThreadDetailsItem { .. } | ClientSettingChange::ResetThreadDetailsItems => {
                unreachable!("thread details visibility applies to the current record")
            }
            ClientSettingChange::ToolCallVisibility(visibility) => ("toolCallVisibility", text(visibility.as_str())),
            ClientSettingChange::InterfaceFontSize(size) => {
                ("fontSizeInterface", Value::from((*size).clamp(MIN_INTERFACE_FONT_SIZE, MAX_INTERFACE_FONT_SIZE)))
            }
            ClientSettingChange::ComposerRichText(on) => ("composerRichTextEnabled", Value::Bool(*on)),
            ClientSettingChange::SendShortcut(shortcut) => (
                "sendShortcut",
                text(match shortcut {
                    SendShortcut::Enter => "enter",
                    SendShortcut::ModEnterMultiline => "mod-enter-multiline",
                    SendShortcut::ModEnter => "mod-enter",
                }),
            ),
            ClientSettingChange::FollowUpBehavior(behavior) => (
                "followUpBehavior",
                text(match behavior {
                    FollowUpBehavior::Queue => "queue",
                    FollowUpBehavior::Steer => "steer",
                }),
            ),
            ClientSettingChange::ShowSkillsInSlashMenu(on) => ("showSkillsInSlashMenu", Value::Bool(*on)),
            ClientSettingChange::Theme(theme) => (THEME_KEY, text(theme.as_str())),
            ClientSettingChange::AppearanceContrast(value) => ("appearanceContrast", Value::from((*value).clamp(50,200))),
            ClientSettingChange::GlassOpacity(value) => ("glassOpacity", Value::from((*value).clamp(40,100))),
            ClientSettingChange::ThemePatch(_) => unreachable!("theme patches apply atomically"),
            ClientSettingChange::ThemeLibrary(value) => (crate::themes::LIBRARY_KEY, value.clone()),
            ClientSettingChange::ThemeHalves(value) => (crate::themes::HALVES_KEY, serde_json::json!(value)),
            ClientSettingChange::AppearanceMode(mode) => (THEME_APPEARANCE_MODE_KEY, text(mode.as_str())),
            ClientSettingChange::LastEnabledProjectGrouping(mode) => {
                ("t3code:last-enabled-project-grouping-mode", text(mode.as_str()))
            }
            ClientSettingChange::OnboardingCompletedAt(at) => ("onboardingCompletedAt", text(at)),
            ClientSettingChange::VoiceMicrophones(entries) => (
                "voiceMicrophones",
                serde_json::to_value(entries).unwrap_or_else(|_| Value::Array(Vec::new())),
            ),
            ClientSettingChange::VoiceShortcutMode(mode) => ("voiceShortcutMode", text(mode.as_str())),
            ClientSettingChange::SpeechLanguage(language) => ("speechLanguage", text(language)),
            ClientSettingChange::SpeechTranslateToEnglish(on) => ("speechTranslateToEnglish", Value::Bool(*on)),
            ClientSettingChange::SpeechSilenceFiltering(on) => ("speechSilenceFiltering", Value::Bool(*on)),
            ClientSettingChange::SpeechRemoveFillerWords(on) => ("speechRemoveFillerWords", Value::Bool(*on)),
            ClientSettingChange::SpeechCustomWords(words) => (
                "speechCustomWords",
                serde_json::to_value(words).unwrap_or_else(|_| Value::Array(Vec::new())),
            ),
            ClientSettingChange::SpeechPostProcessingPrompt(prompt) => (
                "speechPostProcessingPrompt",
                serde_json::to_value(prompt).unwrap_or(Value::Null),
            ),
            ClientSettingChange::SpeechCorrectionWord(word) => ("speechCorrectionWord", text(word)),
            ClientSettingChange::Language(language) => ("language", text(language)),
            ClientSettingChange::ProactivePanelsEnabled(on) => ("proactivePanelsEnabled", Value::Bool(*on)),
            ClientSettingChange::ScreenReaderAnnouncements(mode) => (
                "screenReaderAnnouncements",
                text(screen_reader_announcements_value(*mode)),
            ),
            ClientSettingChange::ScreenReaderWindowTitle(on) => ("screenReaderWindowTitle", Value::Bool(*on)),
        }
    }
}

/// The settings file: every key as read, with typed views over the ones the native app uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClientSettingsDoc {
    entries: Map<String, Value>,
}

impl ClientSettingsDoc {
    /// A missing, unreadable, or non-object file reads as all defaults.
    pub fn from_json(text: &str) -> ClientSettingsDoc {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(entries)) => ClientSettingsDoc { entries },
            _ => ClientSettingsDoc::default(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.entries).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn entries(&self) -> &Map<String, Value> {
        &self.entries
    }

    /// Applies one edit. Returns false when the stored value is already that.
    pub fn apply(&mut self, change: &ClientSettingChange) -> bool {
        !self.apply_change(change).is_empty()
    }

    /// Applies one edit and returns the keys it changed: the patch to save onto the file.
    pub fn apply_change(&mut self, change: &ClientSettingChange) -> Map<String, Value> {
        if let ClientSettingChange::ProjectGroupingOverride { key, mode } = change {
            let key = crate::json_text::js_trim(key);
            if key.is_empty() { return Map::new(); }
            let mut overrides = self.project_grouping_settings().overrides;
            if let Some(mode) = mode {
                overrides.insert(key.to_owned(), crate::sidebar::grouping::GroupingMode::parse(mode.as_str()).unwrap_or_default());
            } else {
                overrides.remove(key);
            }
            let value = Value::Object(overrides.into_iter().map(|(key, mode)| (key, Value::String(mode.as_str().to_owned()))).collect());
            let key = "sidebarProjectGroupingOverrides";
            if self.entries.get(key) == Some(&value) { return Map::new(); }
            self.entries.insert(key.to_owned(), value.clone());
            return [(key.to_owned(), value)].into_iter().collect();
        }
        if let ClientSettingChange::ThreadDetailsItem { id, visible } = change {
            let mut visibility = self.thread_details_visibility();
            visibility.set_with_legacy_usage(id, *visible, Some(self.thread_details_usage_enabled()));
            let value = Value::Object(
                visibility
                    .overrides()
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::Bool(*v)))
                    .collect(),
            );
            let key = "threadDetailsItems";
            if self.entries.get(key) == Some(&value) {
                return Map::new();
            }
            self.entries.insert(key.to_owned(), value.clone());
            return [(key.to_owned(), value)].into_iter().collect();
        }
        if let ClientSettingChange::ResetThreadDetailsItems = change {
            let mut patch = Map::new();
            let empty_items = Value::Object(Map::new());
            if self.entries.get("threadDetailsItems") != Some(&empty_items)
                && (self.entries.contains_key("threadDetailsItems") || !self.thread_details_is_default())
            {
                patch.insert("threadDetailsItems".to_owned(), empty_items);
            }
            if self.thread_details_usage_enabled() || self.entries.get("threadDetailsUsageEnabled") == Some(&Value::Bool(true)) {
                patch.insert("threadDetailsUsageEnabled".to_owned(), Value::Bool(false));
            }
            self.entries.extend(patch.clone());
            return patch;
        }
        if let ClientSettingChange::ThemePatch(patch) = change {
            let patch: Map<String, Value> = patch.iter().filter(|(k,v)|self.entries.get(*k)!=Some(*v)).map(|(k,v)|(k.clone(),v.clone())).collect();
            self.entries.extend(patch.clone()); return patch;
        }
        let mut patch = Map::new();
        let (key, value) = change.entry();
        if let ClientSettingChange::ResetFont(role) = change {
            let size = Value::from(role.default_size());
            if self.entries.get(role.size_key()) != Some(&size) {
                patch.insert(role.size_key().to_owned(), size);
            }
        }
        if let ClientSettingChange::Theme(_) = change {
            patch.insert(crate::themes::HALVES_KEY.to_owned(), serde_json::json!({}));
            let mode = Value::String(self.appearance_mode().as_str().to_owned());
            if self.entries.get(THEME_APPEARANCE_MODE_KEY) != Some(&mode) {
                patch.insert(THEME_APPEARANCE_MODE_KEY.to_owned(), mode);
            }
        }
        if self.entries.get(key) == Some(&value) && patch.is_empty() {
            return patch;
        }
        patch.insert(key.to_owned(), value);
        self.entries.extend(patch.clone());
        patch
    }

    /// The composer's view of the same file.
    pub fn composer(&self) -> ClientSettings {
        ClientSettings::from_json(&serde_json::to_string(&self.entries).unwrap_or_default())
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.entries.get(key).and_then(Value::as_str)
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        self.entries.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    fn literal<T: Default>(&self, key: &str, parse: fn(&str) -> Option<T>) -> T {
        self.text(key).and_then(parse).unwrap_or_default()
    }

    /// Keeps the layout saved by native builds that stored it with panel state.
    pub fn migrate_diff_layout(&mut self, legacy_panels: &Value) -> Map<String, Value> {
        if self.entries.contains_key("diffLayout") { return Map::new(); }
        let layout = legacy_panels.get("diffLayout").and_then(|value| serde_json::from_value(value.clone()).ok());
        layout.map(|layout| self.apply_change(&ClientSettingChange::DiffLayout(layout))).unwrap_or_default()
    }

    pub fn diff_layout(&self) -> crate::diff::Layout {
        self.entries
            .get("diffLayout")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    pub fn diff_ignore_whitespace(&self) -> bool {
        self.flag("diffIgnoreWhitespace", true)
    }

    pub fn diff_files_collapsed(&self) -> bool {
        self.flag("diffFilesCollapsed", true)
    }

    pub fn diff_color_scheme(&self) -> DiffColorScheme {
        self.literal("diffColorScheme", DiffColorScheme::parse)
    }

    pub fn browser_link_target(&self) -> BrowserLinkTarget { self.literal("browserLinkTarget", BrowserLinkTarget::parse) }
    pub fn pull_request_links_open_in_app(&self) -> bool { self.flag("pullRequestLinksOpenInApp", true) }

    pub fn browser_search_engine(&self) -> BrowserSearchEngine {
        self.literal("browserSearchEngine", BrowserSearchEngine::parse)
    }

    pub fn browser_custom_search_url(&self) -> &str {
        self.text("browserCustomSearchUrl")
            .filter(|url| url.chars().count() <= 2048)
            .unwrap_or("")
    }

    pub fn browser_search_template(&self) -> &str {
        match self.browser_search_engine() {
            BrowserSearchEngine::DuckDuckGo => crate::preview::address::DEFAULT_SEARCH_TEMPLATE,
            BrowserSearchEngine::Google => "https://www.google.com/search?q=%s",
            BrowserSearchEngine::Bing => "https://www.bing.com/search?q=%s",
            BrowserSearchEngine::Brave => "https://search.brave.com/search?q=%s",
            BrowserSearchEngine::Kagi => "https://kagi.com/search?q=%s",
            BrowserSearchEngine::Ecosia => "https://www.ecosia.org/search?q=%s",
            BrowserSearchEngine::Custom
                if valid_search_template(self.browser_custom_search_url()) =>
            {
                self.browser_custom_search_url().trim()
            }
            BrowserSearchEngine::Custom => crate::preview::address::DEFAULT_SEARCH_TEMPLATE,
        }
    }

    pub fn notification_mode(&self) -> NotificationMode {
        self.entries.get("notificationMode").and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default()
    }

    pub fn in_app_notifications(&self) -> bool {
        self.flag("inAppNotificationsEnabled", false)
    }

    pub fn project_grouping(&self) -> ProjectGroupingMode {
        self.literal("sidebarProjectGroupingMode", ProjectGroupingMode::parse)
    }

    pub fn project_grouping_settings(&self) -> crate::sidebar::grouping::GroupingSettings {
        use crate::sidebar::grouping::{GroupingMode, GroupingSettings};
        let overrides = self
            .entries
            .get("sidebarProjectGroupingOverrides")
            .and_then(Value::as_object)
            .and_then(|entries| {
                entries
                    .iter()
                    .map(|(key, value)| {
                        let key = crate::json_text::js_trim(key);
                        if key.is_empty() {
                            return None;
                        }
                        Some((key.to_owned(), GroupingMode::parse(value.as_str()?)?))
                    })
                    .collect::<Option<_>>()
            })
            .unwrap_or_default();
        GroupingSettings { mode: GroupingMode::parse(self.project_grouping().as_str()).unwrap_or_default(), overrides, ..Default::default() }
    }

    /// `readLastEnabledProjectGroupingMode`: only the path mode is remembered.
    pub fn last_enabled_project_grouping(&self) -> ProjectGroupingMode {
        match self.text("t3code:last-enabled-project-grouping-mode") {
            Some("repository_path") => ProjectGroupingMode::RepositoryPath,
            _ => ProjectGroupingMode::Repository,
        }
    }

    pub fn project_sort_order(&self) -> ProjectSortOrder {
        self.literal("sidebarProjectSortOrder", ProjectSortOrder::parse)
    }

    pub fn thread_sort_order(&self) -> crate::sidebar::sort::ThreadSortOrder {
        match self.text("sidebarThreadSortOrder") {
            Some("created_at") => crate::sidebar::sort::ThreadSortOrder::CreatedAt,
            _ => crate::sidebar::sort::ThreadSortOrder::UpdatedAt,
        }
    }

    pub fn boring_sidebar_enabled(&self) -> bool { self.flag("boringSidebarEnabled", false) }

    pub fn boring_sidebar_multiline_rows(&self) -> bool { self.flag("boringSidebarMultilineRows", false) }

    pub fn working_shelf(&self) -> bool {
        self.flag("sidebarWorkingShelfEnabled", true)
    }

    pub fn confirm_thread_unpin(&self) -> bool {
        self.flag("confirmThreadUnpin", false)
    }

    pub fn confirm_thread_archive(&self) -> bool {
        self.flag("confirmThreadArchive", false)
    }

    pub fn confirm_thread_delete(&self) -> bool {
        self.flag("confirmThreadDelete", true)
    }

    /// `QuitConfirmationModeSetting`: older files stored a boolean (`true` holds).
    pub fn confirm_quit(&self) -> QuitConfirmationMode {
        match self.entries.get("confirmQuit") {
            Some(Value::Bool(true)) => QuitConfirmationMode::Hold,
            Some(Value::Bool(false)) => QuitConfirmationMode::Direct,
            Some(Value::String(mode)) => QuitConfirmationMode::parse(mode).unwrap_or_default(),
            _ => QuitConfirmationMode::default(),
        }
    }

    pub fn timestamp_format(&self) -> TimestampFormat {
        self.literal("timestampFormat", TimestampFormat::parse)
    }

    pub fn chat_width(&self) -> ChatWidth {
        self.literal("chatWidth", ChatWidth::parse)
    }

    pub fn effort_display(&self) -> EffortDisplay {
        self.literal("effortDisplay", EffortDisplay::parse)
    }

    pub fn panel_animation_duration_ms(&self) -> u32 {
        self.entries
            .get("panelAnimationDurationMs")
            .and_then(Value::as_u64)
            .and_then(|ms| u32::try_from(ms).ok())
            .filter(|ms| (MIN_PANEL_ANIMATION_DURATION_MS..=MAX_PANEL_ANIMATION_DURATION_MS).contains(ms))
            .unwrap_or(DEFAULT_PANEL_ANIMATION_DURATION_MS)
    }

    pub fn environment_identification_mode(&self) -> EnvironmentIdentificationMode {
        self.literal("environmentIdentificationMode", EnvironmentIdentificationMode::parse)
    }

    pub fn persist_composer_context_strip(&self) -> bool {
        self.flag("persistComposerContextStrip", false)
    }

    pub fn thread_details_usage_enabled(&self) -> bool {
        self.flag("threadDetailsUsageEnabled", false)
    }

    pub fn thread_details_items(&self) -> std::collections::HashMap<String, bool> {
        self.entries
            .get("threadDetailsItems")
            .and_then(Value::as_object)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_bool()?)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn thread_details_visibility(&self) -> crate::thread_details::Visibility {
        crate::thread_details::Visibility::from_overrides(self.thread_details_items())
    }

    pub fn thread_details_item_visible(&self, id: &str) -> bool {
        self.thread_details_visibility()
            .shows_with_legacy_usage(id, Some(self.thread_details_usage_enabled()))
    }

    pub fn thread_details_is_default(&self) -> bool {
        self.thread_details_visibility()
            .is_default_with_legacy_usage(Some(self.thread_details_usage_enabled()))
    }

    pub fn thread_details_enabled_count(&self) -> usize {
        crate::thread_details::ITEMS
            .iter()
            .filter(|(id, _, _)| self.thread_details_item_visible(id))
            .count()
    }

    pub fn tool_call_visibility(&self) -> ToolCallVisibility {
        self.literal("toolCallVisibility", ToolCallVisibility::parse)
    }

    /// `InterfaceFontSize`: an integer from 12 to 20; anything else is the default.
    pub fn interface_font_size(&self) -> u32 {
        self.entries
            .get("fontSizeInterface")
            .and_then(Value::as_u64)
            .and_then(|size| u32::try_from(size).ok())
            .filter(|size| (MIN_INTERFACE_FONT_SIZE..=MAX_INTERFACE_FONT_SIZE).contains(size))
            .unwrap_or(DEFAULT_INTERFACE_FONT_SIZE)
    }

    pub fn composer_rich_text(&self) -> bool {
        self.flag("composerRichTextEnabled", true)
    }

    pub fn show_skills_in_slash_menu(&self) -> bool {
        self.flag("showSkillsInSlashMenu", true)
    }

    pub fn appearance_contrast(&self) -> u32 { self.entries.get("appearanceContrast").and_then(Value::as_u64).filter(|n| (50..=200).contains(n)).unwrap_or(100) as u32 }
    pub fn glass_opacity(&self) -> u32 { self.entries.get("glassOpacity").and_then(Value::as_u64).filter(|n| (40..=100).contains(n)).unwrap_or(80) as u32 }

    /// `readThemePreference`: unknown ids are kept, so a theme this build lacks is not lost.
    pub fn theme(&self) -> ThemePreference {
        self.text(THEME_KEY)
            .filter(|value| !value.is_empty())
            .map(ThemePreference::parse)
            .unwrap_or(ThemePreference::Default(AppearanceMode::System))
    }

    /// `readAppearanceModePreference`: the stored mode, else the default theme's own mode.
    /// `onboardingCompletedAt !== null`. Installs from before onboarding read as not completed.
    pub fn onboarding_completed(&self) -> bool {
        self.text("onboardingCompletedAt").is_some()
    }

    pub fn appearance_mode(&self) -> AppearanceMode {
        if let Some(mode) = self.text(THEME_APPEARANCE_MODE_KEY).and_then(AppearanceMode::parse) {
            return mode;
        }
        match self.theme() {
            ThemePreference::Default(mode) => mode,
            ThemePreference::Named(_) => AppearanceMode::Light,
        }
    }

    /// Opt-in TeX math rendering (`mathRenderingEnabled` in `contracts/settings.ts`, false by default).
    pub fn math_rendering_enabled(&self) -> bool {
        self.flag("mathRenderingEnabled", false)
    }

    pub fn legacy_voice_microphone(&self) -> &str {
        self.text("voiceMicrophone").unwrap_or("")
    }

    pub fn voice_microphones(&self) -> Vec<crate::voice_input::VoiceMicrophoneEntry> {
        self.entries
            .get("voiceMicrophones")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    pub fn voice_shortcut_mode(&self) -> crate::voice_input::VoiceShortcutMode {
        self.literal("voiceShortcutMode", crate::voice_input::VoiceShortcutMode::parse)
    }

    pub fn speech_language(&self) -> &str {
        self.text("speechLanguage")
            .filter(|lang| !lang.trim().is_empty())
            .unwrap_or("auto")
    }

    pub fn speech_translate_to_english(&self) -> bool {
        self.flag("speechTranslateToEnglish", false)
    }

    pub fn speech_silence_filtering(&self) -> bool {
        self.flag("speechSilenceFiltering", true)
    }

    pub fn speech_remove_filler_words(&self) -> bool {
        self.flag("speechRemoveFillerWords", true)
    }

    pub fn speech_custom_words(&self) -> Vec<crate::voice_input::SpeechCustomWordEntry> {
        self.entries
            .get("speechCustomWords")
            .and_then(|value| serde_json::from_value::<Vec<crate::voice_input::SpeechCustomWordEntry>>(value.clone()).ok())
            .filter(|entries| crate::voice_input::validate_speech_custom_words(entries).is_ok())
            .unwrap_or_default()
    }

    pub fn speech_post_processing_prompt(&self) -> crate::voice_input::SpeechPostProcessingPromptSettings {
        self.entries
            .get("speechPostProcessingPrompt")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    pub fn speech_correction_word(&self) -> &str {
        self.text("speechCorrectionWord").unwrap_or("")
    }

    /// UI language (`language` in `packages/contracts/src/settings.ts:583`, default `"en"`).
    pub fn language(&self) -> &str {
        self.text("language").unwrap_or(crate::i18n::DEFAULT_LOCALE)
    }

    /// Proactively open linked PR or Changes panels (`proactivePanelsEnabled`, default `false`).
    pub fn proactive_panels_enabled(&self) -> bool {
        self.flag("proactivePanelsEnabled", false)
    }

    /// Open-thread screen reader verbosity (`screenReaderAnnouncements`, default `"status"`).
    pub fn screen_reader_announcements(&self) -> ScreenReaderAnnouncements {
        crate::announcements::screen_reader_announcements_setting(&self.entries)
    }

    /// Lead window title with thread status for screen readers (`screenReaderWindowTitle`, default `false`).
    pub fn screen_reader_window_title(&self) -> bool {
        crate::announcements::screen_reader_window_title_setting(&self.entries)
    }
}

/// `projectGroupingModeFromToggle`.
pub fn project_grouping_from_toggle(enabled: bool, last_enabled: ProjectGroupingMode) -> ProjectGroupingMode {
    match (enabled, last_enabled) {
        (false, _) => ProjectGroupingMode::Separate,
        (true, ProjectGroupingMode::RepositoryPath) => ProjectGroupingMode::RepositoryPath,
        (true, _) => ProjectGroupingMode::Repository,
    }
}

/// The settings file after a patch, and whether the file it replaces must be kept aside first.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedSettings {
    pub text: String,
    pub backup: bool,
}

/// Lays a patch over the file as it is now, not as this instance loaded it, so another
/// instance's saves survive. A file that is not a settings object is never patched in place:
/// it is backed up and replaced by the patch alone.
pub fn merge_patch(current: Option<&str>, patch: &Map<String, Value>) -> MergedSettings {
    let current = current.filter(|text| !text.trim().is_empty());
    let (mut entries, backup) = match current.map(serde_json::from_str::<Value>) {
        None => (Map::new(), false),
        Some(Ok(Value::Object(entries))) => (entries, false),
        Some(_) => (Map::new(), true),
    };
    entries.extend(patch.clone());
    MergedSettings { text: serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "{}".to_owned()), backup }
}

#[cfg(test)]
mod tests;

/// `isValidSearchUrlTemplate` from `shared/src/preview.ts`.
pub fn valid_search_template(template: &str) -> bool {
    let template = template.trim();
    if !template.contains("%s") {
        return false;
    }
    let (Ok(first), Ok(second)) = (
        url::Url::parse(&template.replace("%s", "a")),
        url::Url::parse(&template.replace("%s", "b")),
    ) else {
        return false;
    };
    matches!(first.scheme(), "http" | "https")
        && first.username() == second.username()
        && first.password() == second.password()
        && first.host_str() == second.host_str()
        && first.port() == second.port()
}
