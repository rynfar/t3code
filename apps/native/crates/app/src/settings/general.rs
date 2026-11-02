//! General, Chat and Labs: per-device settings from `GeneralSettingsPanel`, `ChatSettingsPanel`
//! and `LegacyFeaturesSection` in `web/src/components/settings/SettingsPanels.tsx`, with
//! `NotificationSettings.tsx`. Copy and defaults match the web.

use gpui::{App, Context, Entity, IntoElement, SharedString};
use t3_core::client_settings::{
    ClientSettingChange, ClientSettingsDoc, NOTIFICATION_MODES, ProjectGroupingMode, ProjectSortOrder, QuitConfirmationMode,
    TimestampFormat, notification_mode_value, project_grouping_from_toggle,
};
use t3_core::composer::{FollowUpBehavior, SendShortcut};

use super::controls::{Row, Select, SelectOption, Switch, section};
use super::{Block, SettingsView, store};
use crate::app_state::AppState;

const MODIFIER: &str = if cfg!(target_os = "macos") { "\u{2318}" } else { "Ctrl" };

fn literal_options<T: Copy>(all: &[T], value: fn(T) -> &'static str, label: fn(T) -> &'static str) -> Vec<SelectOption> {
    all.iter().map(|item| SelectOption::new(value(*item), label(*item))).collect()
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
            "Notifications",
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
                Row::new("in-app-notifications", "In-app notifications")
                    .description("Show a toast when another thread finishes, fails, or needs input or approval while this app has focus.")
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
            "Sidebar",
            vec![
                Row::new("project-grouping", "Project grouping")
                    .description("Combine matching repositories across environments.")
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
                Row::new("project-order", "Project order")
                    .description("Order of projects in the sidebar project picker and command palette.")
                    .highlighted(self.is_target("project-order"))
                    .reset(
                        doc.project_sort_order() != ProjectSortOrder::default(),
                        reset(state, ClientSettingChange::ProjectSortOrder(ProjectSortOrder::default())),
                    )
                    .control(
                        Select::new("project-order-select", literal_options(ProjectSortOrder::ALL, ProjectSortOrder::as_str, ProjectSortOrder::label))
                            .selected(doc.project_sort_order().as_str())
                            .width(176.)
                            .on_change(move |value, _, cx| {
                                if let Some(order) = ProjectSortOrder::parse(&value) {
                                    store::update_client(&order_state, ClientSettingChange::ProjectSortOrder(order), cx);
                                }
                            }),
                    )
                    .into_any_element(),
                Row::new("working-shelf", "Working section")
                    .description("Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you. The boring sidebar in Labs has no Working section.")
                    .highlighted(self.is_target("working-shelf"))
                    .reset(!doc.working_shelf(), reset(state, ClientSettingChange::WorkingShelf(true)))
                    .control(switch(state, "working-shelf-switch", doc.working_shelf(), ClientSettingChange::WorkingShelf))
                    .into_any_element(),
            ],
            cx,
        );

        let quit_state = state.clone();
        let confirmations = section(
            "Confirmations",
            vec![
                Row::new("unpin-confirmation", "Unpin confirmation")
                    .description("Ask before unpinning a thread from the pinned section.")
                    .highlighted(self.is_target("unpin-confirmation"))
                    .reset(doc.confirm_thread_unpin(), reset(state, ClientSettingChange::ConfirmThreadUnpin(false)))
                    .control(switch(state, "unpin-confirmation-switch", doc.confirm_thread_unpin(), ClientSettingChange::ConfirmThreadUnpin))
                    .into_any_element(),
                Row::new("archive-confirmation", "Archive confirmation")
                    .description("Require a second click on the inline archive action before a thread is archived.")
                    .highlighted(self.is_target("archive-confirmation"))
                    .reset(doc.confirm_thread_archive(), reset(state, ClientSettingChange::ConfirmThreadArchive(false)))
                    .control(switch(state, "archive-confirmation-switch", doc.confirm_thread_archive(), ClientSettingChange::ConfirmThreadArchive))
                    .into_any_element(),
                Row::new("delete-confirmation", "Delete confirmation")
                    .description("Ask before deleting a thread and its chat history.")
                    .highlighted(self.is_target("delete-confirmation"))
                    .reset(!doc.confirm_thread_delete(), reset(state, ClientSettingChange::ConfirmThreadDelete(true)))
                    .control(switch(state, "delete-confirmation-switch", doc.confirm_thread_delete(), ClientSettingChange::ConfirmThreadDelete))
                    .into_any_element(),
                Row::new("quit-confirmation", "Quit shortcut")
                    .description("Hold mode also quits on two quick presses.")
                    .highlighted(self.is_target("quit-confirmation"))
                    .reset(
                        doc.confirm_quit() != QuitConfirmationMode::default(),
                        reset(state, ClientSettingChange::ConfirmQuit(QuitConfirmationMode::default())),
                    )
                    .control(
                        Select::new(
                            "quit-confirmation-select",
                            literal_options(QuitConfirmationMode::ALL, QuitConfirmationMode::as_str, QuitConfirmationMode::label),
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

        let time_state = state.clone();
        let display = section(
            "Display",
            vec![
                Row::new("time-format", "Time format")
                    .description("System default follows your browser or OS clock preference.")
                    .highlighted(self.is_target("time-format"))
                    .reset(
                        doc.timestamp_format() != TimestampFormat::default(),
                        reset(state, ClientSettingChange::TimestampFormat(TimestampFormat::default())),
                    )
                    .control(
                        Select::new("time-format-select", literal_options(TimestampFormat::ALL, TimestampFormat::as_str, TimestampFormat::label))
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

        vec![
            Block::new(&["notifications", "thread-notifications", "in-app-notifications"], notifications),
            Block::new(&["sidebar", "project-grouping", "project-order", "working-shelf"], sidebar),
            Block::new(&["confirmations", "unpin-confirmation", "archive-confirmation", "delete-confirmation", "quit-confirmation"], confirmations),
            Block::new(&["display", "time-format"], display),
        ]
    }

    pub(super) fn chat(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let composer = self.doc(cx).composer();
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
        let composer_section = section(
            "Composer",
            vec![
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
            ],
            cx,
        );
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
        vec![Block::new(&["composer", "send-shortcut"], composer_section), Block::new(&["follow-ups", "follow-up-behavior"], follow_ups)]
    }

    pub(super) fn labs(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let composer = self.doc(cx).composer();
        let legacy = section(
            "Legacy",
            vec![
                Row::new("legacy-plan-mode", "Plan mode (legacy)")
                    .description("Restore Build/Plan, /plan, /default, and Shift+Tab. Off uses build mode.")
                    .highlighted(self.is_target("legacy-plan-mode"))
                    .control(switch(&self.state, "legacy-plan-mode-switch", composer.plan_mode_enabled, ClientSettingChange::PlanMode))
                    .into_any_element(),
                Row::new("legacy-context-window-indicator", "Context window indicator (legacy)")
                    .description("Shows context window usage as a circular indicator in the composer.")
                    .highlighted(self.is_target("legacy-context-window-indicator"))
                    .control(switch(&self.state, "legacy-context-window-switch", composer.context_window_meter, ClientSettingChange::ContextWindowMeter))
                    .into_any_element(),
            ],
            cx,
        );
        vec![Block::new(&["legacy-features", "legacy-plan-mode", "legacy-context-window-indicator"], legacy)]
    }
}
