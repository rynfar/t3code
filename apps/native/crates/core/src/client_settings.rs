//! Per-device settings the web keeps in local storage: `ClientSettingsSchema` in
//! `contracts/src/settings.ts`, plus the theme preferences `web/src/hooks/useTheme.ts` stores
//! under `t3code:theme` and `t3code:theme-appearance-mode`. The file keeps every key it does not
//! model, so a newer build's settings survive an older one, and an invalid value reads as the
//! contract's default without dropping the rest. Edits are saved as patches over the file as it
//! is at write time, never as this instance's whole copy.

use serde_json::{Map, Value};

use crate::composer::ClientSettings;
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
    /// `SidebarProjectGroupingMode`. The switch shows on for both repository modes.
    ProjectGroupingMode {
        Repository => "repository": "Repository",
        RepositoryPath => "repository_path": "Repository path",
        Separate => "separate": "Separate",
    } default Repository
);

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
    NotificationMode(NotificationMode),
    InAppNotifications(bool),
    ProjectGrouping(ProjectGroupingMode),
    ProjectSortOrder(ProjectSortOrder),
    WorkingShelf(bool),
    ConfirmThreadUnpin(bool),
    ConfirmThreadArchive(bool),
    ConfirmThreadDelete(bool),
    ConfirmQuit(QuitConfirmationMode),
    TimestampFormat(TimestampFormat),
    ChatWidth(ChatWidth),
    InterfaceFontSize(u32),
    ComposerRichText(bool),
    SendShortcut(crate::composer::SendShortcut),
    FollowUpBehavior(crate::composer::FollowUpBehavior),
    ShowSkillsInSlashMenu(bool),
    PlanMode(bool),
    ContextWindowMeter(bool),
    /// `setTheme`: replaces the theme and keeps the current appearance mode.
    Theme(ThemePreference),
    AppearanceMode(AppearanceMode),
    /// The last repository mode the grouping switch turned off, restored when it turns back on.
    LastEnabledProjectGrouping(ProjectGroupingMode),
}

impl ClientSettingChange {
    fn entry(&self) -> (&'static str, Value) {
        use crate::composer::{FollowUpBehavior, SendShortcut};
        let text = |value: &str| Value::String(value.to_owned());
        match self {
            ClientSettingChange::NotificationMode(mode) => ("notificationMode", text(notification_mode_value(*mode))),
            ClientSettingChange::InAppNotifications(on) => ("inAppNotificationsEnabled", Value::Bool(*on)),
            ClientSettingChange::ProjectGrouping(mode) => ("sidebarProjectGroupingMode", text(mode.as_str())),
            ClientSettingChange::ProjectSortOrder(order) => ("sidebarProjectSortOrder", text(order.as_str())),
            ClientSettingChange::WorkingShelf(on) => ("sidebarWorkingShelfEnabled", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadUnpin(on) => ("confirmThreadUnpin", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadArchive(on) => ("confirmThreadArchive", Value::Bool(*on)),
            ClientSettingChange::ConfirmThreadDelete(on) => ("confirmThreadDelete", Value::Bool(*on)),
            ClientSettingChange::ConfirmQuit(mode) => ("confirmQuit", text(mode.as_str())),
            ClientSettingChange::TimestampFormat(format) => ("timestampFormat", text(format.as_str())),
            ClientSettingChange::ChatWidth(width) => ("chatWidth", text(width.as_str())),
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
            ClientSettingChange::PlanMode(on) => ("planModeEnabled", Value::Bool(*on)),
            ClientSettingChange::ContextWindowMeter(on) => ("contextWindowMeterEnabled", Value::Bool(*on)),
            ClientSettingChange::Theme(theme) => (THEME_KEY, text(theme.as_str())),
            ClientSettingChange::AppearanceMode(mode) => (THEME_APPEARANCE_MODE_KEY, text(mode.as_str())),
            ClientSettingChange::LastEnabledProjectGrouping(mode) => {
                ("t3code:last-enabled-project-grouping-mode", text(mode.as_str()))
            }
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
        let mut patch = Map::new();
        let (key, value) = change.entry();
        if self.entries.get(key) == Some(&value) {
            return patch;
        }
        if let ClientSettingChange::Theme(_) = change {
            let mode = Value::String(self.appearance_mode().as_str().to_owned());
            if self.entries.get(THEME_APPEARANCE_MODE_KEY) != Some(&mode) {
                patch.insert(THEME_APPEARANCE_MODE_KEY.to_owned(), mode);
            }
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

    pub fn notification_mode(&self) -> NotificationMode {
        self.entries.get("notificationMode").and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default()
    }

    pub fn in_app_notifications(&self) -> bool {
        self.flag("inAppNotificationsEnabled", false)
    }

    pub fn project_grouping(&self) -> ProjectGroupingMode {
        self.literal("sidebarProjectGroupingMode", ProjectGroupingMode::parse)
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

    /// `readThemePreference`: unknown ids are kept, so a theme this build lacks is not lost.
    pub fn theme(&self) -> ThemePreference {
        self.text(THEME_KEY)
            .filter(|value| !value.is_empty())
            .map(ThemePreference::parse)
            .unwrap_or(ThemePreference::Default(AppearanceMode::System))
    }

    /// `readAppearanceModePreference`: the stored mode, else the default theme's own mode.
    pub fn appearance_mode(&self) -> AppearanceMode {
        if let Some(mode) = self.text(THEME_APPEARANCE_MODE_KEY).and_then(AppearanceMode::parse) {
            return mode;
        }
        match self.theme() {
            ThemePreference::Default(mode) => mode,
            ThemePreference::Named(_) => AppearanceMode::Light,
        }
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
