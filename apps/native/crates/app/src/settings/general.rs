//! General, Chat and Labs: per-device and server-scoped settings from `GeneralSettingsPanel`,
//! `ChatSettingsPanel` (`web/src/components/settings/SettingsPanels.tsx`),
//! `NotificationSettings.tsx`, and `LabsSettingsPanel.tsx`. Copy and defaults match the web.

use gpui::{App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Task, div};
use gpui_component::h_flex;
use t3_core::client_settings::{
    ClientSettingChange, ClientSettingsDoc, NOTIFICATION_MODES, ProjectGroupingMode, ProjectSortOrder, QuitConfirmationMode,
    SCREEN_READER_ANNOUNCEMENTS_MODES, ScreenReaderAnnouncements, TimestampFormat, ToolCallVisibility,
    notification_mode_value, project_grouping_from_toggle, screen_reader_announcements_label,
    screen_reader_announcements_value,
};
use t3_core::composer::{FollowUpBehavior, SendShortcut};
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::i18n::{
    DEFAULT_LOCALE, SUPPORTED_LOCALES, SYSTEM_LANGUAGE, native_language_name, supported_language_value,
};
use t3_core::managed_policy::{self, Capability};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::search::Section;

use super::controls::{
    Button, ButtonSize, ButtonVariant, InlineButton, InlineTone, Row, Select, SelectOption, Switch, r, section,
};
use super::{Block, SettingsView, store};
use crate::app_state::AppState;
use crate::i18n::t;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const MODIFIER: &str = if cfg!(target_os = "macos") { "\u{2318}" } else { "Ctrl" };
const SIMLOCK_URL: &str = "https://incubator.callstack.com/simlock/";

const DEVICE_DEFAULT_KEYS: &[&str] = &[
    t3_core::client_settings::THEME_KEY, t3_core::client_settings::THEME_APPEARANCE_MODE_KEY,
    t3_core::themes::HALVES_KEY, "appearanceContrast", "glassOpacity", "diffColorScheme",
    "chatWidth", "effortDisplay", "mathRenderingEnabled", "panelAnimationDurationMs",
    "environmentIdentificationMode", "language", "timestampFormat", "notificationMode",
    "inAppNotificationsEnabled", "sidebarProjectGroupingMode", "sidebarProjectSortOrder",
    "sidebarWorkingShelfEnabled", "wordWrap", "persistComposerContextStrip", "diffFilesCollapsed",
    "diffIgnoreWhitespace", "diffLayout", "proactivePanelsEnabled", "showSkillsInSlashMenu",
    "composerRichTextEnabled", "sendShortcut", "followUpBehavior", "toolCallVisibility",
    "screenReaderAnnouncements", "screenReaderWindowTitle", "confirmThreadArchive",
    "confirmTerminalClose", "confirmThreadDelete", "confirmThreadUnpin", "confirmQuit",
    "fontFamilySans", "fontFamilyComposer", "fontFamilyCode", "fontFamilyTerminal",
    "fontSizeInterface", "fontSizePrompt", "fontSizeCode", "fontSizeTerminal",
    "browserDefaultViewport", "browserDefaultZoomFactor", "browserDefaultAppearance",
    "browserRecordingFrameRate", "browserRecordingShowKeyPresses", "browserRecordingShowMousePresses",
    "browserLinkTarget", "browserSearchEngine", "browserCustomSearchUrl",
    "browserAutoShowFloatingPreview", "pullRequestLinksOpenInApp",
];

fn device_default_patch(doc: &ClientSettingsDoc) -> serde_json::Map<String, serde_json::Value> {
    DEVICE_DEFAULT_KEYS.iter().copied()
        .filter(|key| doc.entries().get(*key).is_some_and(|value| !value.is_null()))
        .map(|key| (key.to_owned(), serde_json::Value::Null))
        .collect()
}

#[derive(Default)]
pub(crate) struct ChatState {
    advanced: bool,
    fast_checkout: Option<FastCheckoutSupport>,
}

struct FastCheckoutSupport {
    owners: Vec<EnvironmentOwner>,
    connection: Option<u64>,
    supported: bool,
    _task: Task<()>,
}

fn advertised_fast_checkout(reply: &serde_json::Value) -> bool {
    reply["environment"]["capabilities"]["fastWorktreeCheckout"].as_bool() == Some(true)
}

fn literal_options<T: Copy>(all: &[T], value: fn(T) -> &'static str, label: fn(T) -> &'static str) -> Vec<SelectOption> {
    all.iter().map(|item| SelectOption::new(value(*item), label(*item))).collect()
}

fn translated_literal_options<T: Copy>(
    all: &[T],
    value: fn(T) -> &'static str,
    label: fn(T) -> &'static str,
    cx: &App,
) -> Vec<SelectOption> {
    all.iter().map(|item| SelectOption::new(value(*item), t(cx, label(*item)))).collect()
}

/// A switch bound to one client setting.
fn switch(state: &Entity<AppState>, id: &'static str, checked: bool, change: fn(bool) -> ClientSettingChange) -> Switch {
    let state = state.clone();
    Switch::new(id, checked).on_change(move |checked, _, cx| store::update_client(&state, change(checked), cx))
}

fn reset(state: &Entity<AppState>, change: ClientSettingChange) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static {
    let state = state.clone();
    move |_, _, cx| store::update_client(&state, change.clone(), cx)
}

impl SettingsView {
    fn doc<'a>(&self, cx: &'a App) -> &'a ClientSettingsDoc {
        store::client(cx)
    }

    pub(super) fn general(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = self.doc(cx).clone();
        let state = &self.state;

        let notification_state = state.clone();
        let notifications = section(
            t(cx, "Notifications"),
            vec![
                Row::new("thread-notifications", "Thread notifications")
                    .description("System alerts when a thread finishes, fails, or needs input or approval. Applies to this device while T3 Code is open.")
                    .highlighted(self.is_target("thread-notifications"))
                    .control(
                        Select::new("thread-notifications-select", literal_options(&NOTIFICATION_MODES, notification_mode_value, |mode| mode.label()))
                            .selected(notification_mode_value(doc.notification_mode()))
                            .width(224.)
                            .on_change(move |value, _, cx| {
                                if let Some(mode) = NOTIFICATION_MODES.into_iter().find(|mode| notification_mode_value(*mode) == value.as_ref()) {
                                    store::update_client(&notification_state, ClientSettingChange::NotificationMode(mode), cx);
                                }
                            }),
                    )
                    .into_any_element(),
                Row::new("in-app-notifications", t(cx, "In-app notifications"))
                    .description(t(cx, "Show a toast when another thread finishes, fails, or needs input or approval while this app has focus."))
                    .highlighted(self.is_target("in-app-notifications"))
                    .control(switch(state, "in-app-notifications-switch", doc.in_app_notifications(), ClientSettingChange::InAppNotifications))
                    .into_any_element(),
            ],
            cx,
        );

        let grouping = doc.project_grouping();
        let last_enabled = doc.last_enabled_project_grouping();
        let grouping_state = state.clone();
        let order_state = state.clone();
        let sidebar = section(
            t(cx, "Sidebar"),
            vec![
                Row::new("project-grouping", t(cx, "Project grouping"))
                    .description(t(cx, "Combine matching repositories across environments."))
                    .highlighted(self.is_target("project-grouping"))
                    .reset(grouping != ProjectGroupingMode::default(), reset(state, ClientSettingChange::ProjectGrouping(ProjectGroupingMode::default())))
                    .control(Switch::new("project-grouping-switch", grouping != ProjectGroupingMode::Separate).on_change(move |checked, _, cx| {
                        if !checked && grouping != ProjectGroupingMode::Separate {
                            store::update_client(&grouping_state, ClientSettingChange::LastEnabledProjectGrouping(grouping), cx);
                        }
                        let remembered = if checked { last_enabled } else { grouping };
                        store::update_client(&grouping_state, ClientSettingChange::ProjectGrouping(project_grouping_from_toggle(checked, remembered)), cx);
                    }))
                    .into_any_element(),
                Row::new("project-order", t(cx, "Project order"))
                    .description(t(cx, "Order of projects in the sidebar project picker and command palette."))
                    .highlighted(self.is_target("project-order"))
                    .reset(
                        doc.project_sort_order() != ProjectSortOrder::default(),
                        reset(state, ClientSettingChange::ProjectSortOrder(ProjectSortOrder::default())),
                    )
                    .control(
                        Select::new("project-order-select", translated_literal_options(ProjectSortOrder::ALL, ProjectSortOrder::as_str, ProjectSortOrder::label, cx))
                            .selected(doc.project_sort_order().as_str())
                            .width(176.)
                            .on_change(move |value, _, cx| {
                                if let Some(order) = ProjectSortOrder::parse(&value) {
                                    store::update_client(&order_state, ClientSettingChange::ProjectSortOrder(order), cx);
                                }
                            }),
                    )
                    .into_any_element(),
                Row::new("working-shelf", t(cx, "Working section"))
                    .description(t(cx, "Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you. The boring sidebar in Labs has no Working section."))
                    .highlighted(self.is_target("working-shelf"))
                    .reset(!doc.working_shelf(), reset(state, ClientSettingChange::WorkingShelf(true)))
                    .control(switch(state, "working-shelf-switch", doc.working_shelf(), ClientSettingChange::WorkingShelf))
                    .into_any_element(),
            ],
            cx,
        );

        let quit_state = state.clone();
        let confirmations = section(
            t(cx, "Confirmations"),
            vec![
                Row::new("unpin-confirmation", t(cx, "Unpin confirmation"))
                    .description(t(cx, "Ask before unpinning a thread from the pinned section."))
                    .highlighted(self.is_target("unpin-confirmation"))
                    .reset(doc.confirm_thread_unpin(), reset(state, ClientSettingChange::ConfirmThreadUnpin(false)))
                    .control(switch(state, "unpin-confirmation-switch", doc.confirm_thread_unpin(), ClientSettingChange::ConfirmThreadUnpin))
                    .into_any_element(),
                Row::new("archive-confirmation", t(cx, "Archive confirmation"))
                    .description(t(cx, "Require a second click on the inline archive action before a thread is archived."))
                    .highlighted(self.is_target("archive-confirmation"))
                    .reset(doc.confirm_thread_archive(), reset(state, ClientSettingChange::ConfirmThreadArchive(false)))
                    .control(switch(state, "archive-confirmation-switch", doc.confirm_thread_archive(), ClientSettingChange::ConfirmThreadArchive))
                    .into_any_element(),
                Row::new("delete-confirmation", t(cx, "Delete confirmation"))
                    .description(t(cx, "Ask before deleting a thread and its chat history."))
                    .highlighted(self.is_target("delete-confirmation"))
                    .reset(!doc.confirm_thread_delete(), reset(state, ClientSettingChange::ConfirmThreadDelete(true)))
                    .control(switch(state, "delete-confirmation-switch", doc.confirm_thread_delete(), ClientSettingChange::ConfirmThreadDelete))
                    .into_any_element(),
                Row::new("terminal-close-confirmation", t(cx, "Terminal close confirmation"))
                    .description(t(cx, "Ask before closing terminals and stopping their running processes."))
                    .highlighted(self.is_target("terminal-close-confirmation"))
                    .reset(!doc.confirm_terminal_close(), reset(state, ClientSettingChange::ConfirmTerminalClose(true)))
                    .control(switch(state, "terminal-close-confirmation-switch", doc.confirm_terminal_close(), ClientSettingChange::ConfirmTerminalClose))
                    .into_any_element(),
                Row::new("quit-confirmation", t(cx, "Quit shortcut"))
                    .description(t(cx, "Hold mode also quits on two quick presses."))
                    .highlighted(self.is_target("quit-confirmation"))
                    .reset(
                        doc.confirm_quit() != QuitConfirmationMode::default(),
                        reset(state, ClientSettingChange::ConfirmQuit(QuitConfirmationMode::default())),
                    )
                    .control(
                        Select::new(
                            "quit-confirmation-select",
                            translated_literal_options(QuitConfirmationMode::ALL, QuitConfirmationMode::as_str, QuitConfirmationMode::label, cx),
                        )
                        .selected(doc.confirm_quit().as_str())
                        .width(160.)
                        .on_change(move |value, _, cx| {
                            if let Some(mode) = QuitConfirmationMode::parse(&value) {
                                store::update_client(&quit_state, ClientSettingChange::ConfirmQuit(mode), cx);
                            }
                        }),
                    )
                    .into_any_element(),
            ],
            cx,
        );

        let lang_state = state.clone();
        let language_value = supported_language_value(doc.language());
        let mut language_options = Vec::with_capacity(SUPPORTED_LOCALES.len() + 1);
        language_options.push(SelectOption::new(SYSTEM_LANGUAGE, t(cx, "System default")));
        for &locale in SUPPORTED_LOCALES {
            language_options.push(SelectOption::new(locale, native_language_name(locale)));
        }
        let time_state = state.clone();
        let display = section(
            t(cx, "Display"),
            vec![
                Row::new("language", t(cx, "Language"))
                    .description(t(cx, "System default follows your browser or OS language."))
                    .highlighted(self.is_target("language"))
                    .reset(
                        doc.language() != DEFAULT_LOCALE,
                        reset(state, ClientSettingChange::Language(DEFAULT_LOCALE.to_owned())),
                    )
                    .control(
                        Select::new("language-select", language_options)
                            .selected(language_value)
                            .width(160.)
                            .on_change(move |value, _, cx| {
                                if !value.is_empty() {
                                    store::update_client(&lang_state, ClientSettingChange::Language(value.to_string()), cx);
                                }
                            }),
                    )
                    .into_any_element(),
                Row::new("time-format", t(cx, "Time format"))
                    .description(t(cx, "System default follows your browser or OS clock preference."))
                    .highlighted(self.is_target("time-format"))
                    .reset(
                        doc.timestamp_format() != TimestampFormat::default(),
                        reset(state, ClientSettingChange::TimestampFormat(TimestampFormat::default())),
                    )
                    .control(
                        Select::new(
                            "time-format-select",
                            translated_literal_options(TimestampFormat::ALL, TimestampFormat::as_str, TimestampFormat::label, cx),
                        )
                        .selected(doc.timestamp_format().as_str())
                        .width(160.)
                        .on_change(move |value, _, cx| {
                            if let Some(format) = TimestampFormat::parse(&value) {
                                store::update_client(&time_state, ClientSettingChange::TimestampFormat(format), cx);
                            }
                        }),
                    )
                    .into_any_element(),
            ],
            cx,
        );

        let has_device_changes = !device_default_patch(&doc).is_empty();
        let restore = section(t(cx, "Device defaults"), vec![
            Row::new("restore-device-defaults", t(cx, "Restore device defaults"))
                .description(t(cx, "Reset display, chat, browser, and notification preferences on this device."))
                .control(Button::new("restore-device-defaults-button", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, "Restore")).disabled(!has_device_changes)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_restore_device_defaults(cx))))
                .into_any_element(),
        ], cx);

        vec![
            Block::new(&["notifications", "thread-notifications", "in-app-notifications"], notifications),
            Block::new(&["sidebar", "project-grouping", "project-order", "working-shelf"], sidebar),
            Block::new(&["confirmations", "unpin-confirmation", "archive-confirmation", "delete-confirmation", "terminal-close-confirmation", "quit-confirmation"], confirmations),
            Block::new(&["display", "language", "time-format"], display),
            Block::new(&["restore-device-defaults"], restore),
        ]
    }

    fn confirm_restore_device_defaults(&self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        crate::dialog::ask_with_options(
            t(cx, "Restore device defaults?
Reset personal preferences on this device. Saved environments, themes, and browser profiles are kept."),
            t(cx, "Restore"),
            crate::dialog::ConfirmVariant::Destructive,
            crate::dialog::InitialFocus::Cancel,
            move |_, cx| {
                cx.defer(move |cx| {
                    let patch = device_default_patch(store::client(cx));
                    if !patch.is_empty() {
                        store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                    }
                });
            },
            None,
            cx,
        );
    }

    pub(super) fn chat(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = self.doc(cx).clone();
        let composer = doc.composer();
        let state = &self.state;
        let shortcut_options = vec![
            SelectOption::new("enter", "Enter"),
            SelectOption::new("mod-enter-multiline", format!("{MODIFIER} + Enter for multiline prompts")),
            SelectOption::new("mod-enter", format!("{MODIFIER} + Enter always")),
        ];
        let shortcut_value = match composer.send_shortcut {
            SendShortcut::Enter => "enter",
            SendShortcut::ModEnterMultiline => "mod-enter-multiline",
            SendShortcut::ModEnter => "mod-enter",
        };
        let follow_up_hint: SharedString = match composer.send_shortcut {
            SendShortcut::ModEnterMultiline => format!(
                "Press {MODIFIER} + Enter for single-line prompts or {MODIFIER} + Shift + Enter for multiline prompts to do the opposite for one message."
            ),
            SendShortcut::ModEnter => format!("Press {MODIFIER} + Shift + Enter to do the opposite for one message."),
            SendShortcut::Enter => format!("Press {MODIFIER} + Enter to do the opposite for one message."),
        }
        .into();
        let shortcut_state = state.clone();
        let follow_up_state = state.clone();
        let advanced = self.chat_ui.advanced || self.is_target("skills-in-slash-menu");
        let weak = cx.entity().downgrade();
        let mut composer_rows = vec![
            Row::new("composer-rich-text", "Rich text composer")
                .description("Show formatted Markdown as you type.")
                .highlighted(self.is_target("composer-rich-text"))
                .reset(!doc.composer_rich_text(), reset(state, ClientSettingChange::ComposerRichText(true)))
                .control(
                    div()
                        .debug_selector(|| "composer-rich-text-switch".to_owned())
                        .child(switch(state, "composer-rich-text-switch", doc.composer_rich_text(), ClientSettingChange::ComposerRichText)),
                )
                .into_any_element(),
            Row::new("send-shortcut", "Send shortcut")
                .description("Choose when Enter sends a prompt or inserts a new line")
                .highlighted(self.is_target("send-shortcut"))
                .reset(composer.send_shortcut != SendShortcut::default(), reset(state, ClientSettingChange::SendShortcut(SendShortcut::default())))
                .control(Select::new("send-shortcut-select", shortcut_options).selected(shortcut_value).popup_width(280.).show_check().on_change(
                    move |value, _, cx| {
                        let shortcut = match value.as_ref() {
                            "enter" => SendShortcut::Enter,
                            "mod-enter-multiline" => SendShortcut::ModEnterMultiline,
                            "mod-enter" => SendShortcut::ModEnter,
                            _ => return,
                        };
                        store::update_client(&shortcut_state, ClientSettingChange::SendShortcut(shortcut), cx);
                    },
                ))
                .into_any_element(),
            div()
                .px(r(16.))
                .py(r(8.))
                .child(
                    Button::new("chat-composer-advanced", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .icon(if advanced { Icon::ChevronDown } else { Icon::ChevronRight })
                        .label("Advanced")
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |view, cx| {
                                view.chat_ui.advanced = !advanced;
                                if view.is_target("skills-in-slash-menu") {
                                    view.target = None;
                                }
                                cx.notify();
                            });
                        }),
                )
                .into_any_element(),
        ];
        if advanced {
            composer_rows.push(
                Row::new("skills-in-slash-menu", "Show skills in slash menu")
                    .description("Also include skills in the / command menu. Skills always appear when you type $.")
                    .highlighted(self.is_target("skills-in-slash-menu"))
                    .reset(!doc.show_skills_in_slash_menu(), reset(state, ClientSettingChange::ShowSkillsInSlashMenu(true)))
                    .control(switch(state, "skills-in-slash-menu-switch", doc.show_skills_in_slash_menu(), ClientSettingChange::ShowSkillsInSlashMenu))
                    .into_any_element(),
            );
        }
        let composer_section = section("Composer", composer_rows, cx);
        let follow_ups = section(
            "Follow-ups",
            vec![
                Row::new("follow-up-behavior", "Follow-up behavior")
                    .description(format!("Queue follow-ups while the agent runs or steer the current run. {follow_up_hint}"))
                    .highlighted(self.is_target("follow-up-behavior"))
                    .reset(
                        composer.follow_up_behavior != FollowUpBehavior::default(),
                        reset(state, ClientSettingChange::FollowUpBehavior(FollowUpBehavior::default())),
                    )
                    .control(
                        Select::new("follow-up-select", vec![SelectOption::new("queue", "Queue"), SelectOption::new("steer", "Steer")])
                            .selected(if composer.follow_up_behavior == FollowUpBehavior::Queue { "queue" } else { "steer" })
                            .show_check()
                            .on_change(move |value, _, cx| {
                                let behavior = match value.as_ref() {
                                    "queue" => FollowUpBehavior::Queue,
                                    "steer" => FollowUpBehavior::Steer,
                                    _ => return,
                                };
                                store::update_client(&follow_up_state, ClientSettingChange::FollowUpBehavior(behavior), cx);
                            }),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        let visibility = doc.tool_call_visibility();
        let visibility_state = state.clone();
        let activity = section(
            "Activity",
            vec![
                Row::new("tool-call-visibility", "Tool calls in chat")
                    .description("Hide the commands, edits, and searches agents run. Hide successful keeps failures. Zen shows only final answers. Approvals, questions, and errors always show.")
                    .highlighted(self.is_target("tool-call-visibility"))
                    .reset(
                        visibility != ToolCallVisibility::default(),
                        reset(state, ClientSettingChange::ToolCallVisibility(ToolCallVisibility::default())),
                    )
                    .control(
                        Select::new(
                            "tool-call-visibility-select",
                            literal_options(ToolCallVisibility::ALL, ToolCallVisibility::as_str, ToolCallVisibility::label),
                        )
                        .selected(visibility.as_str())
                        .width(160.)
                        .on_change(move |value, _, cx| {
                            if let Some(mode) = ToolCallVisibility::parse(&value) {
                                store::update_client(&visibility_state, ClientSettingChange::ToolCallVisibility(mode), cx);
                            }
                        }),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        let sr_announcements = doc.screen_reader_announcements();
        let sr_announcements_state = state.clone();
        let screen_reader = section(
            "Screen reader",
            vec![
                Row::new("screen-reader-announcements", "Screen reader announcements")
                    .description("What a screen reader hears from the open thread: a turn starting, needing you, and ending. Status and messages adds what the agent writes before its last message. Everything adds each step.")
                    .highlighted(self.is_target("screen-reader-announcements"))
                    .reset(
                        sr_announcements != ScreenReaderAnnouncements::default(),
                        reset(state, ClientSettingChange::ScreenReaderAnnouncements(ScreenReaderAnnouncements::default())),
                    )
                    .control(
                        Select::new(
                            "screen-reader-announcements-select",
                            literal_options(
                                &SCREEN_READER_ANNOUNCEMENTS_MODES,
                                screen_reader_announcements_value,
                                screen_reader_announcements_label,
                            ),
                        )
                        .selected(screen_reader_announcements_value(sr_announcements))
                        .width(192.)
                        .on_change(move |value, _, cx| {
                            if let Some(mode) = ScreenReaderAnnouncements::parse(&value) {
                                store::update_client(&sr_announcements_state, ClientSettingChange::ScreenReaderAnnouncements(mode), cx);
                            }
                        }),
                    )
                    .into_any_element(),
                Row::new("screen-reader-window-title", "Screen reader window title")
                    .description("Start the window title with the thread's status, like Needs approval, and its name, so screen readers read them first when you switch apps. Taskbars, shared screens, and time trackers see it too.")
                    .highlighted(self.is_target("screen-reader-window-title"))
                    .reset(doc.screen_reader_window_title(), reset(state, ClientSettingChange::ScreenReaderWindowTitle(false)))
                    .control(switch(
                        state,
                        "screen-reader-window-title-switch",
                        doc.screen_reader_window_title(),
                        ClientSettingChange::ScreenReaderWindowTitle,
                    ))
                    .into_any_element(),
            ],
            cx,
        );
        vec![
            Block::new(&["composer", "composer-rich-text", "send-shortcut", "skills-in-slash-menu"], composer_section),
            Block::new(&["follow-ups", "follow-up-behavior"], follow_ups),
            Block::new(&["activity", "tool-call-visibility"], activity),
            Block::new(&["screen-reader", "screen-reader-announcements", "screen-reader-window-title"], screen_reader),
        ]
    }

    fn fast_checkout_supported(&mut self, cx: &mut Context<Self>) -> bool {
        use t3_core::auth::ORCHESTRATION_READ_SCOPE;

        if self.is_project_scope(cx) {
            self.chat_ui.fast_checkout = None;
            return false;
        }
        let owners = self.scoped_environment_owners(cx);
        let app = self.state.read(cx);
        let connection = app.connection_id();
        let fallback_allowed = self.scope.machine.is_none() && owners.is_empty() && self.connected_now(cx);
        if owners.is_empty() && !fallback_allowed {
            self.chat_ui.fast_checkout = None;
            return false;
        }
        if let Some(probe) = self.chat_ui.fast_checkout.as_ref()
            && probe.owners == owners && probe.connection == connection
        {
            return probe.supported;
        }
        // Read the advertised flag directly while the shared EnvironmentCapabilities type omits it.
        let fallback = fallback_allowed.then(|| app.rpc("server.getConfig", serde_json::json!({})));
        let lookups = owners.iter().map(|owner| app.connections.destination(owner, ORCHESTRATION_READ_SCOPE)).collect::<Vec<_>>();
        let runtime = app.runtime().clone();
        let captured = owners.clone();
        let task = cx.spawn(async move |this, cx| {
            let supported = if let Some(request) = fallback {
                request.await.is_ok_and(|reply| advertised_fast_checkout(&reply))
            } else {
                let reports = futures::future::join_all(lookups.into_iter().map(|lookup| {
                    let runtime = runtime.clone();
                    async move {
                        let Ok(destination) = lookup.await else { return false };
                        runtime.spawn(async move {
                            destination.client.request("server.getConfig", serde_json::json!({})).await
                                .is_ok_and(|reply| advertised_fast_checkout(&reply))
                        }).await.unwrap_or(false)
                    }
                })).await;
                reports.into_iter().all(|supported| supported)
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(probe) = this.chat_ui.fast_checkout.as_mut()
                    && probe.owners == captured && probe.connection == connection
                {
                    probe.supported = supported;
                    cx.notify();
                }
            });
        });
        self.chat_ui.fast_checkout = Some(FastCheckoutSupport { owners, connection, supported: false, _task: task });
        false
    }

    pub(super) fn labs(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let doc = self.doc(cx).clone();
        let config = self.scoped_representative_config(cx);
        let write_unavailable = if config.is_none() || !self.may("settings:write", cx) {
            Some("Reconnect with settings write access to change this setting.".to_owned())
        } else {
            self.env_only_unavailable(cx)
        };
        let can_write = write_unavailable.is_none();
        let weak = cx.entity().downgrade();
        let server_switch = |id: &'static str, checked: bool, disabled: bool, change: fn(bool) -> ServerSettingChange| {
            let weak = weak.clone();
            Switch::new(id, checked)
                .disabled(disabled)
                .on_change(move |checked, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.update_scoped_server(change(checked), cx));
                })
        };

        let boring_enabled = doc.boring_sidebar_enabled();
        let mut boring_row = Row::new("boring-sidebar", "Boring sidebar")
            .description("Group sidebar threads into project folders. Pinned threads stay on top, and Snoozed and Settled stay below the folders.")
            .highlighted(self.is_target("boring-sidebar"))
            .control(switch(&self.state, "boring-sidebar-switch", boring_enabled, ClientSettingChange::BoringSidebarEnabled));
        if boring_enabled {
            boring_row = boring_row.below(
                div()
                    .mt(r(8.))
                    .mb(r(8.))
                    .border_l_2()
                    .border_color(hsla(p.border).opacity(0.6))
                    .pl(r(12.))
                    .child(
                        Row::new("boring-sidebar-multiline", "Two-line folder rows")
                            .description("Show folder threads on two lines: the title, then the branch, pull request, and agent.")
                            .highlighted(self.is_target("boring-sidebar-multiline"))
                            .control(switch(
                                &self.state,
                                "boring-sidebar-multiline-switch",
                                doc.boring_sidebar_multiline_rows(),
                                ClientSettingChange::BoringSidebarMultilineRows,
                            )),
                    ),
            );
        }

        let all_configs = self.scoped_all_configs(cx);
        let scoped_policies: Vec<Option<&serde_json::Value>> = if all_configs.is_empty() {
            vec![config.as_ref().and_then(|c| c.managed_policy.as_ref())]
        } else {
            all_configs.iter().map(|c| c.managed_policy.as_ref()).collect()
        };
        let guest_enabled = config.as_ref().and_then(|c| c.settings.guest_sharing_enabled).unwrap_or(false);
        let guest_locked = managed_policy::setting_lock_reason_for_any(
            scoped_policies.iter().copied(),
            &["guestSharingEnabled"],
        );
        let connect_denial = if guest_enabled {
            None
        } else {
            managed_policy::first_managed_denial(scoped_policies.iter().copied(), |policy| {
                managed_policy::capability_denial(policy, Capability::Connect)
            })
        };
        let guest_unavailable = guest_locked.or(connect_denial).or_else(|| write_unavailable.clone());
        let access_link = cx.entity().downgrade();
        let guest_row = Row::new("guest-links", "Guest links")
            .description_content(
                h_flex()
                    .flex_wrap()
                    .gap_x(r(4.))
                    .child("Share a thread with a guest who watches it live in a browser and sends messages you approve. Needs T3 Connect: sign in and link this machine in")
                    .child(InlineButton::new("labs-open-access", "Sharing & access", InlineTone::Muted).on_click(move |_, _, cx| {
                        let _ = access_link.update(cx, |view, cx| view.navigate(Section::Access, None, cx));
                    }))
                    .child("Turning it off stops every guest link."),
            )
            .highlighted(self.is_target("guest-links"))
            .unavailable(guest_unavailable.clone())
            .control(server_switch(
                "guest-links-switch",
                guest_enabled,
                guest_unavailable.is_some(),
                ServerSettingChange::GuestSharingEnabled,
            ));

        let cloud = &self.state.read(cx).cloud;
        let sign_in_denied = scoped_policies.iter().copied()
            .find_map(|policy| managed_policy::capability_denial(policy, Capability::Connect));
        let guest_row = if cloud.identity.is_none() && sign_in_denied.is_none() {
            Row::new("guest-links", t(cx, "Guest links"))
                .description(t(cx, "Share a thread with a guest who watches it live in a browser and sends messages you approve. Sign in to T3 Connect to use guest links."))
                .highlighted(self.is_target("guest-links"))
                .control(Button::new("labs-guest-links-sign-in", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, "Sign in"))
                    .disabled(cloud.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.sign_in_cloud(cx)))))
        } else {
            guest_row
        };

        let linked_enabled = config.as_ref().is_some_and(|c| c.settings.threads.linked_environments_enabled);
        let linked_unavailable = managed_policy::setting_lock_reason_for_any(
            scoped_policies.iter().copied(),
            &["linkedEnvironmentsEnabled"],
        )
        .or_else(|| write_unavailable.clone());
        let connections_link = cx.entity().downgrade();
        let linked_row = Row::new("linked-environments-beta", "Linked environments")
            .description_content(
                h_flex()
                    .flex_wrap()
                    .gap_x(r(4.))
                    .child("Let agents here work on your other machines, and theirs here. Turn it on for each machine, then link them in")
                    .child(InlineButton::new("labs-open-connections", "Connections", InlineTone::Muted).on_click(move |_, _, cx| {
                        let _ = connections_link.update(cx, |view, cx| view.navigate(Section::Connections, None, cx));
                    }))
                    .child("While off, nothing new starts over a link; running tasks finish."),
            )
            .highlighted(self.is_target("linked-environments-beta"))
            .unavailable(linked_unavailable.clone())
            .control(server_switch(
                "linked-environments-switch",
                linked_enabled,
                !can_write || linked_unavailable.is_some(),
                ServerSettingChange::LinkedEnvironmentsEnabled,
            ));

        let prompt_suggestions_enabled = config.as_ref().is_some_and(|c| c.settings.prompt_suggestions_enabled);
        let prompt_unavailable = managed_policy::setting_lock_reason_for_any(
            scoped_policies.iter().copied(),
            &["promptSuggestionsEnabled"],
        )
        .or_else(|| write_unavailable.clone());
        let prompt_suggestions_row = Row::new("prompt-suggestions-beta", "Suggest next prompt")
            .description("Show a predicted next message in the empty composer after a Claude or Codex turn. Tab uses it. Each is an extra cached request on your plan. Needs Codex 0.165+. Claude threads update on restart.")
            .highlighted(self.is_target("prompt-suggestions-beta"))
            .unavailable(prompt_unavailable.clone())
            .control(server_switch(
                "prompt-suggestions-switch",
                prompt_suggestions_enabled,
                !can_write || prompt_unavailable.is_some(),
                ServerSettingChange::PromptSuggestionsEnabled,
            ));

        let simlock_enabled = config.as_ref().is_some_and(|c| c.settings.threads.simlock_device_hosts_enabled);
        let simlock_unavailable = managed_policy::setting_lock_reason_for_any(
            scoped_policies.iter().copied(),
            &["simlockDeviceHostsEnabled"],
        )
        .or_else(|| write_unavailable.clone());
        let devices_link = cx.entity().downgrade();
        let simlock_row = Row::new("simlock-hosts", "Simlock device hosts")
            .description_content(
                h_flex()
                    .flex_wrap()
                    .gap_x(r(4.))
                    .child("Let the Device panel lease simulators and emulators from your own")
                    .child(InlineButton::new("labs-open-simlock", "Simlock", InlineTone::Muted).on_click(|_, _, cx| cx.open_url(SIMLOCK_URL)))
                    .child("pool, added in")
                    .child(InlineButton::new("labs-open-devices", "Devices", InlineTone::Muted).on_click(move |_, _, cx| {
                        let _ = devices_link.update(cx, |view, cx| view.navigate(Section::Devices, None, cx));
                    }))
                    .child("An alpha that may change or break. T3 Code never installs Simlock."),
            )
            .highlighted(self.is_target("simlock-hosts"))
            .unavailable(simlock_unavailable.clone())
            .control(server_switch(
                "simlock-hosts-switch",
                simlock_enabled,
                !can_write || simlock_unavailable.is_some(),
                ServerSettingChange::SimlockDeviceHostsEnabled,
            ));

        let mut rows = vec![
            boring_row.into_any_element(),
            guest_row.into_any_element(),
            linked_row.into_any_element(),
            prompt_suggestions_row.into_any_element(),
            simlock_row.into_any_element(),
        ];

        if self.fast_checkout_supported(cx) {
            let fast_checkout = config.as_ref().is_some_and(|c| c.settings.threads.fast_worktree_checkout);
            let fast_unavailable = managed_policy::setting_lock_reason_for_any(
                scoped_policies.iter().copied(),
                &["fastWorktreeCheckout"],
            )
            .or(write_unavailable);
            rows.push(
                Row::new("worktree-fast-checkout", "Fast worktree checkout")
                    .description("On a Mac, clone new worktrees from a checkout cached in the T3 home folder instead of writing every file. Uses Git\u{2019}s normal checkout when it can\u{2019}t. Turning it off deletes the cache.")
                    .highlighted(self.is_target("worktree-fast-checkout"))
                    .unavailable(fast_unavailable.clone())
                    .control(div().debug_selector(|| "worktree-fast-checkout-switch".to_owned()).child(server_switch(
                        "worktree-fast-checkout-switch",
                        fast_checkout,
                        !can_write || fast_unavailable.is_some(),
                        ServerSettingChange::FastWorktreeCheckout,
                    )))
                    .into_any_element(),
            );
        }

        let beta = section("Beta", rows, cx);
        vec![Block::new(
            &[
                "beta",
                "boring-sidebar",
                "boring-sidebar-multiline",
                "guest-links",
                "linked-environments-beta",
                "prompt-suggestions-beta",
                "simlock-hosts",
                "worktree-fast-checkout",
            ],
            beta,
        )]
    }
}

#[cfg(test)]
mod device_defaults_tests {
    use super::*;

    #[test]
    fn fast_worktree_checkout_requires_an_explicit_capability() {
        for capabilities in [serde_json::json!({}), serde_json::json!({"fastWorktreeCheckout": false})] {
            assert!(!advertised_fast_checkout(&serde_json::json!({"environment": {"capabilities": capabilities}})));
        }
        assert!(advertised_fast_checkout(&serde_json::json!({"environment": {"capabilities": {"fastWorktreeCheckout": true}}})));
    }

    #[test]
    fn device_restore_only_patches_its_preferences() {
        let doc = ClientSettingsDoc::from_json(r#"{
            "wordWrap": true, "language": "de", "notificationMode": null,
            "browserProfiles": [ "profile" ], "onboardingCompletedAt": "today", "futureKey": true
        }"#);
        let patch = device_default_patch(&doc);
        assert_eq!(serde_json::Value::Object(patch), serde_json::json!({"wordWrap": null, "language": null}));
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div};
    use serde_json::json;
    use t3_core::settings::search::Section;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn general_chat_and_labs_render_with_platform_and_policy_gates(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "settings:write"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "platform": { "os": "darwin", "arch": "arm64" }
                        },
                        "managedPolicy": {
                            "organization": "Acme Corp",
                            "limits": { "connect": false }
                        },
                        "settings": {
                            "guestSharingEnabled": false,
                            "linkedEnvironmentsEnabled": true,
                            "promptSuggestionsEnabled": true,
                            "simlockDeviceHostsEnabled": false,
                            "fastWorktreeCheckout": true
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        for section in [Section::General, Section::Chat, Section::Labs] {
            vcx.update(|_, cx| crate::settings::open(section, cx));
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
        }
        // Fast worktree checkout switch is present on macOS.
        assert!(vcx.debug_bounds("worktree-fast-checkout-switch").is_some());

        // Switching platform to Linux hides the macOS-only Fast worktree checkout row.
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Linux Box",
                            "platform": { "os": "linux", "arch": "x64" }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("worktree-fast-checkout-switch").is_none());

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }

    #[gpui::test]
    fn managed_policy_notices_render_across_browser_connections_and_providers(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "platform": { "os": "darwin", "arch": "arm64" }
                        },
                        "managedPolicy": {
                            "organization": "Acme Corp",
                            "lockedSettings": ["enableAgentBrowserAccess", "guestSharingEnabled", "providerInstances"],
                            "limits": {
                                "connect": false,
                                "browserCookieImport": false,
                                "providers": {
                                    "allowedDrivers": ["codex"],
                                    "lockedInstanceFields": {
                                        "codex": ["environment", "config.setupMode"]
                                    }
                                }
                            }
                        },
                        "providers": [{
                            "instanceId": "codex",
                            "driver": "codex",
                            "enabled": true,
                            "installed": true,
                            "status": "ready",
                            "auth": { "status": "authenticated" },
                            "models": [{ "slug": "gpt-5", "name": "GPT-5" }]
                        }],
                        "settings": {
                            "enableAgentBrowserAccess": true,
                            "guestSharingEnabled": false
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        vcx.update(|_, cx| crate::settings::open(Section::Browser, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("managed-browser-cookie-import-notice").is_some(),
            "Browser settings must render ManagedNotice when browserCookieImport is denied"
        );

        vcx.update(|_, cx| crate::settings::open(Section::Connections, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("managed-connect-notice").is_some(),
            "Connections settings must render ManagedNotice when connect is denied"
        );

        vcx.update(|_, cx| crate::settings::open(Section::Providers, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("managed-provider-environment-notice").is_some(),
            "Provider editor must render ManagedNotice when whole environment is locked"
        );

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    fn managed_locked_settings_lock_rows_across_server_settings_pages(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "platform": { "os": "darwin", "arch": "arm64" },
                            "capabilities": {
                                "storageCleanup": true,
                                "storageCleanupRun": true,
                                "worktreesDirectory": true
                            }
                        },
                        "managedPolicy": {
                            "organization": "Acme Corp",
                            "lockedSettings": [
                                "enableProviderUpdateChecks",
                                "backgroundActivity",
                                "storageCleanup.logsAfterDays",
                                "defaultRuntimeMode",
                                "snoozePresets",
                                "worktreesDirectory",
                                "branchNamingMode",
                                "storageCleanup.worktreeOnDelete",
                                "defaultAutoPull",
                                "sourceControlWritingStyle",
                                "speechProjectCustomWords"
                            ],
                            "limits": {
                                "maxRuntimeMode": "auto-accept-edits"
                            }
                        },
                        "providers": [{
                            "instanceId": "codex",
                            "driver": "codex",
                            "enabled": true,
                            "installed": true,
                            "status": "ready",
                            "auth": { "status": "authenticated" },
                            "models": [{ "slug": "gpt-5", "name": "GPT-5" }]
                        }],
                        "settings": {}
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        let open_and_draw = |vcx: &mut gpui::VisualTestContext, section: Section| {
            vcx.update(|_, cx| crate::settings::open(section, cx));
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
        };

        open_and_draw(&mut vcx, Section::Maintenance);
        assert!(vcx.debug_bounds("provider-update-checks-unavailable").is_some());
        assert!(vcx.debug_bounds("background-activity-unavailable").is_some());
        assert!(vcx.debug_bounds("storage-logs-unavailable").is_some());

        open_and_draw(&mut vcx, Section::Threads);
        assert!(vcx.debug_bounds("default-permissions-unavailable").is_some());
        assert!(vcx.debug_bounds("snooze-options-below-unavailable").is_some());

        open_and_draw(&mut vcx, Section::Worktrees);
        assert!(vcx.debug_bounds("storage-worktrees-location-unavailable").is_some());
        assert!(vcx.debug_bounds("worktree-branch-naming-unavailable").is_some());
        assert!(vcx.debug_bounds("worktree-on-delete-unavailable").is_some());

        open_and_draw(&mut vcx, Section::SourceControl);
        assert!(vcx.debug_bounds("automatic-pull-unavailable").is_some());
        assert!(vcx.debug_bounds("source-control-writing-style-unavailable").is_some());

        open_and_draw(&mut vcx, Section::Voice);
        assert!(vcx.debug_bounds("speech-project-dictionary-below-unavailable").is_some());

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    fn chat_rich_text_switch_toggles_and_resets_client_setting(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open(Section::Chat, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        assert!(vcx.read(|cx| crate::settings::store::client(cx).composer_rich_text()));
        let switch_center = vcx
            .debug_bounds("composer-rich-text-switch")
            .expect("live Rich text composer switch bounds")
            .center();
        vcx.simulate_click(switch_center, Default::default());
        vcx.run_until_parked();
        assert!(!vcx.read(|cx| crate::settings::store::client(cx).composer_rich_text()));

        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.simulate_click(switch_center, Default::default());
        vcx.run_until_parked();
        assert!(vcx.read(|cx| crate::settings::store::client(cx).composer_rich_text()));

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
