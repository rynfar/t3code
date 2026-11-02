//! Composer rules that do not touch the UI: client settings, how a send meets a running turn,
//! which key sends, the permission modes, the new thread's title, and a draft's defaults.
//! Ports `client-runtime/src/state/composerDispatch.ts`, `composerRequiresModifier` and
//! `composerSubmissionIntentForKey` from `web/src/composer-logic.ts`,
//! `web/src/components/chat/runtimeModeConfig.ts`, `resolveComposerInteractionMode` from
//! `web/src/components/ChatView.logic.ts`, `deriveThreadTitleSeed` from
//! `client-runtime/src/operations/threadTitle.ts`, and the draft defaults in
//! `web/src/hooks/useHandleNewThread.ts`.

pub mod attachments;
pub mod drafts;
pub mod menu;
pub mod queue;
pub mod stash;

use serde::Deserialize;

use crate::commands::ComposerDispatch;
use crate::model::ModelSelection;
use crate::providers::{self, ProviderEntry};
use crate::server_config::{InteractionMode, RuntimeMode, ServerConfig, ServerProvider};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SendShortcut {
    #[default]
    Enter,
    ModEnterMultiline,
    ModEnter,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowUpBehavior {
    Queue,
    #[default]
    Steer,
}

/// The `ClientSettingsSchema` keys the composer reads. They are per-device on the web, so the
/// native client keeps its own copy with the web's names and defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientSettings {
    pub send_shortcut: SendShortcut,
    pub follow_up_behavior: FollowUpBehavior,
    /// Legacy Build/Plan toggle and Shift+Tab. Off sends every turn in build mode.
    pub plan_mode_enabled: bool,
    pub notification_mode: crate::notifications::NotificationMode,
    /// Toasts for other threads while the window is focused.
    pub in_app_notifications_enabled: bool,
    /// Skills also show in the `/` menu as `/skill:name`.
    pub show_skills_in_slash_menu: bool,
}

impl Default for ClientSettings {
    fn default() -> Self {
        ClientSettings {
            send_shortcut: SendShortcut::default(),
            follow_up_behavior: FollowUpBehavior::default(),
            plan_mode_enabled: false,
            notification_mode: Default::default(),
            in_app_notifications_enabled: false,
            show_skills_in_slash_menu: true,
        }
    }
}

impl ClientSettings {
    /// Reads a settings file; unknown or invalid keys fall back to the defaults.
    pub fn from_json(text: &str) -> ClientSettings {
        let Ok(serde_json::Value::Object(entries)) = serde_json::from_str::<serde_json::Value>(text) else {
            return ClientSettings::default();
        };
        let mut settings = ClientSettings::default();
        let field = |key: &str| entries.get(key).cloned();
        if let Some(value) = field("sendShortcut").and_then(|v| serde_json::from_value(v).ok()) {
            settings.send_shortcut = value;
        }
        if let Some(value) = field("followUpBehavior").and_then(|v| serde_json::from_value(v).ok()) {
            settings.follow_up_behavior = value;
        }
        if let Some(value) = field("planModeEnabled").and_then(|v| v.as_bool()) {
            settings.plan_mode_enabled = value;
        }
        if let Some(value) = field("notificationMode").and_then(|v| serde_json::from_value(v).ok()) {
            settings.notification_mode = value;
        }
        if let Some(value) = field("inAppNotificationsEnabled").and_then(|v| v.as_bool()) {
            settings.in_app_notifications_enabled = value;
        }
        if let Some(value) = field("showSkillsInSlashMenu").and_then(|v| v.as_bool()) {
            settings.show_skills_in_slash_menu = value;
        }
        settings
    }
}

/// `resolveComposerDispatchMode`. Pending approvals force steer: a send declines them to
/// redirect the waiting turn instead of queueing behind it.
pub fn dispatch_mode(running: bool, alternate: bool, follow_up: FollowUpBehavior, can_steer: bool) -> ComposerDispatch {
    if !running {
        return ComposerDispatch::Auto;
    }
    if !can_steer {
        return ComposerDispatch::Queue;
    }
    match (follow_up, alternate) {
        (FollowUpBehavior::Steer, false) | (FollowUpBehavior::Queue, true) => ComposerDispatch::Steer,
        (FollowUpBehavior::Queue, false) | (FollowUpBehavior::Steer, true) => ComposerDispatch::Queue,
    }
}

/// `composerRequiresModifier`.
pub fn requires_modifier(shortcut: SendShortcut, prompt: &str) -> bool {
    shortcut == SendShortcut::ModEnter || (shortcut == SendShortcut::ModEnterMultiline && prompt.contains(['\r', '\n']))
}

/// `ComposerSubmissionIntent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitIntent {
    Foreground,
    /// Cmd+Enter behind a running turn: the follow-up behavior's opposite.
    Alternate,
    /// Cmd+Enter on a new thread: start it and keep a fresh draft open.
    Background,
}

/// The Enter keystroke as the prompt saw it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnterKey {
    /// Cmd (or Ctrl) was held.
    pub modifier: bool,
    /// The key is held down and the OS repeated it.
    pub repeat: bool,
    /// An input method is composing text, so Enter commits the composition.
    pub composing: bool,
}

/// `composerSubmissionIntentForKey` for Enter with the default keybindings. `None` means the
/// key does not send.
pub fn submit_intent(key: EnterKey, running: bool, draft: bool, shortcut: SendShortcut, prompt: &str) -> Option<SubmitIntent> {
    let EnterKey { modifier, repeat, composing } = key;
    if repeat || composing {
        return None;
    }
    if modifier && running {
        return Some(SubmitIntent::Alternate);
    }
    if modifier && draft {
        return Some(SubmitIntent::Background);
    }
    if requires_modifier(shortcut, prompt) && !modifier {
        return None;
    }
    Some(SubmitIntent::Foreground)
}

/// `runtimeModeConfig`: label and description for each permission mode.
pub fn runtime_mode_label(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "Supervised",
        RuntimeMode::AutoAcceptEdits => "Auto-accept edits",
        RuntimeMode::Auto => "Auto",
        RuntimeMode::FullAccess => "Full access",
    }
}

pub fn runtime_mode_description(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "Ask before commands and file changes.",
        RuntimeMode::AutoAcceptEdits => "Auto-approve edits, ask before other actions.",
        RuntimeMode::Auto => "Supported providers approve routine actions; others still ask.",
        RuntimeMode::FullAccess => "Allow commands and edits without prompts.",
    }
}

/// `resolveCompatibleRuntimeMode`: the modes a provider supports (all when it lists none) and the
/// mode a thread really runs in.
pub fn compatible_runtime_modes(mode: RuntimeMode, provider: Option<&ServerProvider>) -> (Vec<RuntimeMode>, RuntimeMode) {
    let supported = provider.and_then(|provider| provider.supported_runtime_modes.as_ref()).filter(|modes| !modes.is_empty());
    let modes: Vec<RuntimeMode> = match supported {
        Some(supported) => RuntimeMode::ALL.into_iter().filter(|mode| supported.contains(mode)).collect(),
        None => RuntimeMode::ALL.to_vec(),
    };
    let effective = if modes.contains(&mode) { mode } else { modes.first().copied().unwrap_or(mode) };
    (modes, effective)
}

/// `resolveComposerInteractionMode`: plan mode needs the legacy setting and a provider that
/// shows the toggle; otherwise every turn is build mode.
pub fn interaction_mode(plan_mode_enabled: bool, provider: Option<&ServerProvider>, mode: InteractionMode) -> (bool, InteractionMode) {
    let enabled = plan_mode_enabled && provider.is_some_and(|provider| provider.show_interaction_mode_toggle != Some(false));
    (enabled, if enabled { mode } else { InteractionMode::Default })
}

/// `deriveThreadTitleSeed` for a text-only prompt.
pub fn title_seed(text: &str) -> String {
    thread_title_seed(text, &[])
}

/// `deriveThreadTitleSeed`: the prompt's prose, else what is attached, truncated to 50 chars.
pub fn thread_title_seed(text: &str, attachments: &[attachments::Attachment]) -> String {
    let truncate = |text: &str| {
        let characters: Vec<char> = text.trim().chars().collect();
        if characters.len() <= 50 { characters.iter().collect() } else { format!("{}...", characters[..50].iter().collect::<String>()) }
    };
    let prose = menu::title_text(text);
    if !prose.is_empty() {
        return truncate(&prose);
    }
    let images = attachments.iter().filter(|attached| attached.kind == attachments::AttachmentKind::Image).count();
    if images > 0 {
        return if images == 1 { "Attached image".to_owned() } else { "Attached images".to_owned() };
    }
    let name = attachments.first().map(|attached| attached.name.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    if !name.is_empty() {
        return truncate(&format!("File: {name}"));
    }
    "New thread".to_owned()
}

/// What a new thread starts with before the user picks anything.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftDefaults {
    pub model_selection: Option<ModelSelection>,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
}

/// The draft defaults `useNewThreadHandler` resolves: the project's default model (project
/// override, then the environment default), swapped for the fallback model when its quota is
/// spent; else the model the current thread runs; else the first ready provider's default. Then
/// the project's default permissions and the current thread's interaction mode.
pub fn draft_defaults(
    config: Option<&ServerConfig>,
    project_id: &str,
    carry_model: Option<&ModelSelection>,
    carry_interaction: Option<InteractionMode>,
    now_ms: i64,
) -> DraftDefaults {
    let overrides = config.and_then(|config| config.settings.project_settings_overrides.get(project_id));
    let configured_default = match overrides.and_then(|overrides| overrides.default_model_selection.clone()) {
        Some(project_default) => project_default,
        None => config.and_then(|config| config.settings.default_model_selection.clone()),
    }
    .filter(|selection| config.is_none_or(|config| providers::instance_enabled(&config.settings, Some(project_id), &selection.instance_id)));
    let project_default = || config.and_then(|config| providers::project_default_selection(&providers::entries(config, Some(project_id))));
    let checked = configured_default.clone().or_else(|| if carry_model.is_some() { None } else { project_default() });
    let fallback = config
        .map(|config| providers::with_usage_fallback(config, Some(project_id), checked, now_ms))
        .and_then(|(selection, fell_back)| selection.filter(|_| fell_back));
    let model_selection =
        fallback.or(configured_default).or_else(|| carry_model.cloned()).or_else(project_default);
    let runtime_mode = overrides
        .and_then(|overrides| overrides.default_runtime_mode)
        .or_else(|| config.map(|config| config.settings.default_runtime_mode))
        .unwrap_or_default();
    DraftDefaults { model_selection, runtime_mode, interaction_mode: carry_interaction.unwrap_or_default() }
}

/// The selection the composer shows for `entries`: the chosen one while its instance can take
/// work, else the best selectable instance's default model (`resolveDefaultProviderModelSelection`).
pub fn effective_selection(entries: &[ProviderEntry], chosen: Option<&ModelSelection>) -> Option<ModelSelection> {
    let entry = providers::selectable_entry(entries, chosen.map(|selection| selection.instance_id.as_str()))?;
    if let Some(chosen) = chosen.filter(|chosen| chosen.instance_id == entry.instance_id()) {
        return Some(chosen.clone());
    }
    Some(ModelSelection {
        instance_id: entry.instance_id().to_owned(),
        model: providers::default_model(entry.provider)?.to_owned(),
        options: None,
    })
}

#[cfg(test)]
mod tests;
