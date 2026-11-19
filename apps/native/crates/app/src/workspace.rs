//! Window root: titlebar strip, sidebar, and the thread area, or the connect screen. The sidebar
//! resize rail and narrow-window sheet port `components/AppSidebarLayout.tsx` and `SidebarRail`
//! in `components/ui/sidebar.tsx`.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{ ClickEvent, Context, DragMoveEvent, EmptyView, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription, Task, TouchPhase, Window, WindowControlArea, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::status::host_freeze_notice;
use t3_client::supervisor::ConnectionStatus;
use t3_core::keybindings::{WorkspaceFocusArea, next_workspace_focus_area};
use t3_core::pairing::session_has_legacy_permissions;
use t3_core::settings::keybindings::{KeybindingsUpdateToastController, KeybindingsUpdateToastDecision};
use t3_core::settings::quit_hold::swipe::{HistorySwipe, SwipeAction, SwipePhase, SwipeWheel};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::chat_header::{self, ChatHeader};
use crate::composer::{Composer, FocusPrompt, OpenPermissions, ToggleModelPicker};
use crate::connect::ConnectView;
use crate::git::GitDialogs;
use crate::new_thread::{self, DraftHero};
use crate::context_menu::ContextMenu;
use crate::dialog::ConfirmDialog;
use crate::icons::Icon;
use crate::keymap::{
    self, CopyThreadReference, CycleAppearance, FocusNextWorkspaceArea, FocusPreviousWorkspaceArea, JumpToThread1, JumpToThread2, JumpToThread3,
    JumpToThread4, JumpToThread5, JumpToThread6, JumpToThread7, JumpToThread8, JumpToThread9, NextThread, PinThread, PreviousThread,
    RegenerateThreadTitle, SelectTheme, SendAndNewThread, SendBackground, SettleThread, StopThread, ToggleCommandPalette, ToggleFilePicker,
    ToggleProjectSearch, ToggleSidebar, UndoThreadAction, Unimplemented,
};
use crate::palette::{CommandPalette, Mode as PaletteMode};
use crate::right_panel::RightPanelView;
use crate::pull_requests::{CopyPullRequestNumber, LinkPullRequest, LinkPullRequestDialog, PullRequestsPage, ShowLinkedPullRequests};
use crate::snooze_dialog::SnoozeDialog;
use crate::sidebar::SidebarPane;
use crate::terminal::{self, TerminalDrawer, ToggleTerminal};
use crate::theme::{hsla, palette};
use crate::thread_details::{self, DetailsEvent, DetailsView};
use crate::thread_view::ThreadView;
use t3_core::sidebar::layout;

pub const TITLEBAR_HEIGHT: f32 = 52.;
/// Native has no wordmark probe; its sidebar header fits inside `layout::MIN_WIDTH`.
const BRAND_WIDTH: f32 = 0.;

/// The row width the left sidebar occupies: zero while collapsed or shown as a sheet.
pub fn sidebar_inline_width(state: &AppState, viewport_width: f32) -> f32 {
    layout::inline_width(state.sidebar_width, state.sidebar.collapsed, viewport_width, BRAND_WIDTH)
}

struct SidebarResize;

struct WorkspaceAreaFocus {
    sidebar: FocusHandle,
    conversation: FocusHandle,
    composer: FocusHandle,
    terminal: FocusHandle,
    right_panel: FocusHandle,
    home: FocusHandle,
    last_area: Option<WorkspaceFocusArea>,
}

pub struct Workspace {
    state: Entity<AppState>,
    focus_observer: Entity<crate::announcements::FocusObserver>,
    _star_focus: Entity<crate::github_star_prompt::FocusObserver>,
    area_focus: WorkspaceAreaFocus,
    keybindings_toast: KeybindingsUpdateToastController,
    seen_keybindings_updated_seq: u64,
    seen_keybindings_connection: Option<u64>,
    shown_host_freezes: HashSet<(String, u64, Option<u64>, u32)>,
    shown_permission_notices: HashSet<String>,
    history_swipe: HistorySwipe,
    _swipe_clear: Option<Task<()>>,
    last_window_title: Option<String>,
    sidebar: Entity<SidebarPane>,
    thread: Entity<ThreadView>,
    composer: Entity<Composer>,
    lineage: Entity<crate::lineage::ComposerLineage>,
    composer_overlay_height: std::rc::Rc<std::cell::Cell<f32>>,
    composer_replaced: bool,
    draft: Entity<DraftHero>,
    prompt_links: Entity<crate::prompt_links::PromptLinks>,
    home: Entity<crate::home::HomeView>,
    connect: Entity<ConnectView>,
    welcome: Entity<crate::onboarding::WelcomeWizard>,
    menu: Entity<ContextMenu>,
    dialog: Entity<ConfirmDialog>,
    guest_share: Entity<crate::guest_share::GuestShareDialog>,
    palette: Entity<CommandPalette>,
    snooze: Entity<SnoozeDialog>,
    right_panel: Entity<RightPanelView>,
    terminal: Entity<TerminalDrawer>,
    header: Entity<ChatHeader>,
    details: Entity<DetailsView>,
    git_dialogs: Entity<GitDialogs>,
    pull_requests: Entity<PullRequestsPage>,
    link_dialog: Entity<LinkPullRequestDialog>,
    device_setup: Entity<crate::device::DeviceSetupDialog>,
    ssh_prompt: Entity<crate::ssh_prompt::SshPromptDialog>,
    /// A rail drag is live; `has_active_drag` alone also matches row and tab drags.
    rail_dragging: bool,
    _save_sidebar_width: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let guest_share = cx.new(|cx| crate::guest_share::GuestShareDialog::new(state.clone(), window, cx));
        let dialog = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let thread = cx.new(|cx| ThreadView::new(state.clone(), window, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
        thread.update(cx, |view, cx| view.bind_composer(&composer, cx));
        let palette = cx.new(|cx| CommandPalette::new(state.clone(), composer.clone(), window, cx));
        let prompt_links = cx.new(|cx| crate::prompt_links::PromptLinks::new(state.clone(), composer.clone(), window, cx));
        let sidebar = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), dialog.clone(), snooze.clone(), window, cx));
        let lineage = cx.new(|cx| crate::lineage::ComposerLineage::new(state.clone(), cx));
        let draft = cx.new(|cx| DraftHero::new(state.clone(), composer.clone(), cx));
        let home = cx.new(|cx| crate::home::HomeView::new(state.clone(), cx));
        let connect = cx.new(|cx| ConnectView::new(state.clone(), window, cx));
        let welcome = cx.new(|cx| crate::onboarding::WelcomeWizard::new(state.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(state.clone(), window, cx));
        let terminal = cx.new(|cx| TerminalDrawer::new(state.clone(), window, cx));
        let header = cx.new(|cx| ChatHeader::new(state.clone(), menu.clone(), window, cx));
        let details_model = thread_details::init(state.clone(), cx);
        let git = details_model.read(cx).git.clone();
        let git_dialogs = cx.new(|cx| GitDialogs::new(git, window, cx));
        let details = cx.new(|cx| {
            DetailsView::new(
                state.clone(),
                details_model.clone(),
                git_dialogs.clone(),
                terminal.clone(),
                crate::thread_details::DetailsViewPeers {
                    composer: composer.clone(),
                    thread: thread.clone(),
                },
                window,
                cx,
            )
        });
        let pull_requests = cx.new(|cx| PullRequestsPage::new(state.clone(), window, cx));
        let link_dialog = cx.new(|cx| LinkPullRequestDialog::new(state.clone(), window, cx));
        let device_setup = cx.new(|cx| crate::device::DeviceSetupDialog::new(state.clone(), cx));
        let ssh_prompt = cx.new(|cx| crate::ssh_prompt::SshPromptDialog::new(state.clone(), window, cx));
        let focus_observer = cx.new(|cx| crate::announcements::FocusObserver::new(state.clone(), window, cx));
        let star_root = focus_observer.read(cx).root.clone();
        let star_focus = cx.new(|cx| crate::github_star_prompt::FocusObserver::new(state.clone(), &star_root, window, cx));
        let viewport = f32::from(window.viewport_size().width);
        state.update(cx, |state, cx| {
            state.sidebar.collapsed = layout::initial_collapsed(
                t3_client::store::load_ui_state(layout::OPEN_STORAGE_KEY).and_then(|value| value.as_bool()),
            );
            state.set_sidebar_viewport(viewport, cx);
        });
        let (initial_kb_seq, initial_kb_conn) = {
            let s = state.read(cx);
            (s.server_config.keybindings_updated_seq(), s.connection_id())
        };
        let subscriptions = vec![
            cx.subscribe_in(&state, window, |this, state, event: &AppEvent, window, cx| {
                // Navigating from the sheet closes it, as the web's thread and project rows do.
                if matches!(event, AppEvent::Selection) {
                    state.update(cx, |state, cx| state.close_sidebar_sheet(cx));
                }
                if matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                    this.handle_keybindings_config_toast(cx);
                    this.handle_host_freeze_toasts(cx);
                    this.handle_permission_update_toasts(cx);
                }
                if matches!(event, AppEvent::Selection | AppEvent::Shell | AppEvent::Phase | AppEvent::ServerConfig) {
                    this.sync_window_title(window, cx);
                }
                if matches!(event, AppEvent::Phase | AppEvent::Selection | AppEvent::Notice | AppEvent::RightPanel | AppEvent::SidebarVisibility) {
                    cx.notify();
                }
            }),
            cx.observe_window_bounds(window, |this, window, cx| {
                let width = f32::from(window.viewport_size().width);
                this.state.update(cx, |state, cx| state.set_sidebar_viewport(width, cx));
            }),
            cx.observe(&terminal, |_, _, cx| cx.notify()),
            cx.subscribe_in(&composer, window, |this, _, event: &crate::composer::SendAndNewThreadCompleted, window, cx| {
                this.finish_send_and_new_thread(&event.thread_id, window, cx);
            }),
            cx.subscribe(&welcome, |_, _, _: &crate::onboarding::Visibility, cx| cx.notify()),
            cx.subscribe(&details_model, |_, _, _: &DetailsEvent, cx| cx.notify()),
            // The bar ticks while a subagent works; only swapping it for the composer re-lays out.
            cx.observe(&lineage, |this, lineage, cx| {
                let replaced = lineage.read(cx).replaces_composer();
                if replaced != std::mem::replace(&mut this.composer_replaced, replaced) {
                    cx.notify();
                }
            }),
            // Coming back to the app retires its delivered notifications, as the web's focus does.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.state.update(cx, |state, cx| state.clear_pending_notifications(cx));
                } else {
                    crate::voice::handle_window_blur(cx);
                }
            }),
        ];
        let mut workspace = Workspace {
            state,
            focus_observer,
            _star_focus: star_focus,
            area_focus: WorkspaceAreaFocus {
                sidebar: cx.focus_handle(),
                conversation: cx.focus_handle(),
                composer: cx.focus_handle(),
                terminal: cx.focus_handle(),
                right_panel: cx.focus_handle(),
                home: cx.focus_handle(),
                last_area: None,
            },
            keybindings_toast: KeybindingsUpdateToastController::default(),
            seen_keybindings_updated_seq: initial_kb_seq,
            seen_keybindings_connection: initial_kb_conn,
            shown_host_freezes: HashSet::new(),
            shown_permission_notices: HashSet::new(),
            history_swipe: HistorySwipe::default(),
            _swipe_clear: None,
            last_window_title: None,
            sidebar,
            thread,
            composer,
            lineage,
            composer_overlay_height: std::rc::Rc::new(std::cell::Cell::new(0.)),
            composer_replaced: false,
            draft,
            prompt_links,
            home,
            connect,
            welcome,
            menu,
            dialog,
            guest_share,
            palette,
            snooze,
            right_panel,
            terminal,
            header,
            details,
            git_dialogs,
            pull_requests,
            link_dialog,
            device_setup,
            ssh_prompt,
            rail_dragging: false,
            _save_sidebar_width: None,
            _subscriptions: subscriptions,
        };
        workspace.handle_host_freeze_toasts(cx);
        workspace.handle_permission_update_toasts(cx);
        workspace.sync_window_title(window, cx);
        workspace
    }

    /// CN-STATUS: while not connected, a status dot and title with the diagnosis as a tooltip,
    /// and Retry where it can do something.
    fn render_connection_status(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let state = self.state.read(cx);
        let Phase::Online(status) = &state.phase else { return None };
        if *status == ConnectionStatus::Connected {
            return None;
        }
        let host = state.active_environment().map(|environment| environment.label.clone()).unwrap_or_default();
        let details: gpui::SharedString = status.details(&host).into();
        let error_tone = status.summary().1;
        let p = palette(cx);
        Some(
            h_flex()
                .flex_none()
                .gap(px(6.))
                .child(
                    h_flex()
                        .id("connection-status")
                        .gap(px(4.))
                        .child(crate::connect::status_dot(Some(status.dot()), cx))
                        .child(div().text_xs().text_color(if error_tone { hsla(p.destructive_foreground) } else { hsla(p.muted_foreground) }).child(status.title()))
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(details.clone()).build(window, cx)),
                )
                .when(status.can_retry(), |this| {
                    this.child(
                        crate::right_panel::Button::new("connection-retry")
                            .variant(crate::right_panel::Variant::GhostMuted)
                            .size(crate::right_panel::Size::Xs)
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.state.read(cx).retry_connection())),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_titlebar(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let status = self.render_connection_status(cx);
        let state = self.state.read(cx);
        let terminal_toggle = (state.selected_thread.is_some() || state.draft_project.is_some()).then(|| terminal::header_toggle(&self.terminal, cx).into_any_element());
        let theme = cx.theme();
        let collapsed = sidebar_inline_width(state, f32::from(window.viewport_size().width)) == 0.;
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(if collapsed { 130. } else { 20. }))
            .pr(px(12.))
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(self.header.clone())
            .children(status)
            .child(new_thread::new_thread_button(cx))
            .child(chat_header::controls(&self.state, terminal_toggle, window, cx))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_observer.read(cx).root.clone();
        let paired = matches!(self.state.read(cx).phase, Phase::Online(_));
        let theme = cx.theme();
        if !paired || self.state.read(cx).connect.switching {
            // First run shows the welcome wizard where the connect screen would be.
            let screen = if self.welcome.read(cx).is_open() { self.welcome.clone().into_any_element() } else { self.connect.clone().into_any_element() };
            return div()
                .track_focus(&focus)
                .relative()
                .size_full()
                .bg(theme.background)
                .map(crate::quit_hold::listen)
                .capture_key_down(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.star_prompt_activity(cx))))
                .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.star_prompt_activity(cx))))
                .child(crate::github_star_prompt::activity_listener(self.state.clone()))
                .child(screen)
                .child(self.ssh_prompt.clone())
                .children(crate::quit_hold::hint(cx))
                .into_any_element();
        }
        let settings = crate::settings::overlay(window, cx);
        let usage = crate::usage::overlay(window, cx);
        let theme_editor = crate::theme_editor::overlay(window, cx);
        // Settings, Usage and project settings open over the sheet; the web closes it for them.
        if (settings.is_some() || usage.is_some() || theme_editor.is_some()) && self.state.read(cx).sidebar_sheet.is_open() {
            self.state.update(cx, |state, cx| state.close_sidebar_sheet(cx));
        }
        if !cx.has_active_drag() {
            self.rail_dragging = false;
        }
        let swipe_indicator = self.history_swipe.indicator();
        let pane_height = f32::from(window.viewport_size().height);
        self.sync_window_title(window, cx);
        let theme = cx.theme();
        let state = self.state.read(cx);
        let has_thread = state.selected_thread.is_some();
        let has_draft = !has_thread && state.draft_project.is_some();
        let viewport = f32::from(window.viewport_size().width);
        let sidebar_width = sidebar_inline_width(state, viewport);
        let sheet = state.sidebar_sheet.is_open().then(|| layout::sheet_width(viewport));
        let running = state.is_selected_thread_running();
        let panel = state.panel_thread().as_deref().map(|thread| state.panels.panels.panel(thread));
        let right_panel_open = panel.is_some_and(|panel| panel.is_open);
        let preview_open = panel.is_some_and(|panel| {
            panel.is_open && matches!(panel.active().map(|surface| &surface.kind), Some(t3_core::right_panel::SurfaceKind::Preview { .. }))
        });
        let maximized = panel.is_some_and(|panel| panel.is_open && panel.maximized);
        let terminal_open = self.terminal.read(cx).is_open(cx);
        let draft_thread_route = !has_thread && has_draft;
        let composer_requires_modifier = (has_thread || has_draft)
            && t3_core::composer::requires_modifier(state.client_settings.send_shortcut, &self.composer.read(cx).prompt_text(cx));
        let drawer = self.terminal.clone();
        let details_layout = thread_details::layout(&self.state, window, cx);
        let on_pull_requests = state.pull_requests_open;
        let composer_replaced = self.composer_replaced;
        h_flex()
            .track_focus(&focus)
            .relative()
            .size_full()
            .map(crate::quit_hold::listen)
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.state.update(cx, |state, cx| state.star_prompt_activity(cx));
                crate::voice::record_key_down(&event.keystroke.key, event.is_held, cx);
                if event.keystroke.key == "escape"
                    && (crate::voice::handle_escape(window, cx)
                        || crate::voice::handle_setup_escape(cx))
                {
                    cx.stop_propagation();
                }
            }))
            .capture_key_up(cx.listener(|_, event: &gpui::KeyUpEvent, _, cx| {
                crate::voice::handle_key_up(&event.keystroke.key, cx);
            }))
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.star_prompt_activity(cx))))
            .child(crate::github_star_prompt::activity_listener(self.state.clone()))
            .on_action(cx.listener(|_, _: &crate::voice::Dictation, window, cx| {
                if !crate::voice::handle_dictation_action(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleModelPicker, window, cx| {
                this.composer.update(cx, |composer, cx| composer.toggle_picker(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FocusPrompt, window, cx| {
                this.composer.update(cx, |composer, cx| composer.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &crate::composer::OpenWorkspace, window, cx| this.composer.update(cx, |composer, cx| composer.open_workspace(window, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::OpenBranch, window, cx| this.open_composer_branch(window, cx)))
            .on_action(cx.listener(|this, _: &crate::composer::OpenHost, window, cx| this.composer.update(cx, |composer, cx| composer.open_host(window, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::CycleHost, window, cx| this.composer.update(cx, |composer, cx| composer.cycle_host(window, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::UsePreviousWorktree, window, cx| this.composer.update(cx, |composer, cx| composer.use_previous_worktree(window, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::IncreaseEffort, window, cx| this.step_composer_effort(1, window, cx)))
            .on_action(cx.listener(|this, _: &crate::composer::DecreaseEffort, window, cx| this.step_composer_effort(-1, window, cx)))
            .on_action(cx.listener(|this, _: &crate::composer::SendAlternate, window, cx| this.composer.update(cx, |composer, cx| composer.send_alternate(window, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::SteerQueuedMessage, window, cx| {
                if !this.composer.update(cx, |composer, cx| composer.steer_queued_message(window, cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::composer::EditQueuedMessage, window, cx| {
                if !this.composer.update(cx, |composer, cx| composer.edit_queued_message(window, cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &OpenPermissions, window, cx| {
                this.composer.update(cx, |composer, cx| composer.open_permissions(window, cx));
            }))
            .key_context(keymap::root_context_full(
                right_panel_open,
                running,
                terminal_open,
                preview_open,
                draft_thread_route,
                composer_requires_modifier,
            ))
            .on_action(cx.listener(|this, _: &FocusNextWorkspaceArea, window, cx| this.move_workspace_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPreviousWorkspaceArea, window, cx| this.move_workspace_focus(false, window, cx)))
            .on_action(cx.listener(|this, _: &SendBackground, window, cx| this.send_background(window, cx)))
            .on_action(cx.listener(|this, _: &SendAndNewThread, window, cx| this.send_and_new_thread(window, cx)))
            .on_action(cx.listener(|this, _: &crate::right_panel::NewSurface, window, cx| this.open_right_panel_add_menu(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.state.update(cx, |state, cx| state.toggle_sidebar_or_sheet(cx))))
            .on_action(cx.listener(|this, _: &SelectTheme, window, cx| this.palette.update(cx, |palette, cx| palette.open_change_theme(window, cx))))
            .on_action(cx.listener(|this, _: &CycleAppearance, _, cx| this.cycle_appearance(cx)))
            .on_action(cx.listener(|this, _: &StopThread, _, cx| this.state.update(cx, |state, cx| state.stop_selected_thread(cx))))
            .on_action(cx.listener(|this, _: &RegenerateThreadTitle, _, cx| this.regenerate_selected_title(cx)))
            .on_action(cx.listener(|this, _: &PreviousThread, _, cx| this.traverse(false, cx)))
            .on_action(cx.listener(|this, _: &NextThread, _, cx| this.traverse(true, cx)))
            .on_action(cx.listener(|this, _: &SettleThread, _, cx| this.settle_selected(cx)))
            .on_action(cx.listener(|this, _: &PinThread, _, cx| this.pin_selected(cx)))
            .on_action(cx.listener(|this, _: &CopyThreadReference, _, cx| this.copy_selected_reference(cx)))
            .on_action(cx.listener(|this, _: &UndoThreadAction, _, cx| {
                if !this.state.update(cx, |state, cx| state.undo_latest(cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| this.palette.update(cx, |palette, cx| palette.toggle(window, cx))))
            .on_action(cx.listener(|this, _: &keymap::NavigateBack, _, cx| this.state.update(cx, |state, cx| state.navigate_history(true, cx))))
            .on_action(cx.listener(|this, _: &keymap::NavigateForward, _, cx| this.state.update(cx, |state, cx| state.navigate_history(false, cx))))
            .on_action(cx.listener(|this, _: &crate::palette::OpenAddProject, window, cx| {
                this.state.update(cx, |state, cx| state.close_sidebar_sheet(cx));
                this.palette.update(cx, |palette, cx| palette.toggle_mode(PaletteMode::Browse, window, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleFilePicker, window, cx| this.palette.update(cx, |palette, cx| palette.toggle_mode(PaletteMode::Files, window, cx))))
            .on_action(cx.listener(|this, _: &ToggleProjectSearch, window, cx| {
                this.palette.update(cx, |palette, cx| palette.toggle_mode(PaletteMode::Content, window, cx));
            }))
            .on_action(cx.listener(|this, _: &crate::composer::prompt_links::CopyPromptLink, _, cx| this.composer.update(cx, |composer, cx| composer.copy_prompt_link(false, cx))))
            .on_action(cx.listener(|this, _: &crate::composer::prompt_links::CopyPromptLinkWithContext, _, cx| this.composer.update(cx, |composer, cx| composer.copy_prompt_link(true, cx))))
            .on_action(cx.listener(|this, action: &crate::composer::prompt_links::ReplaceLinkedPrompt, window, cx| this.composer.update(cx, |composer, cx| composer.replace_linked_prompt(action, window, cx))))
            .on_action(|_: &Unimplemented, _, cx| cx.propagate())
            .on_action(cx.listener(|this, _: &crate::thread_view::FindInThread, window, cx| this.thread.update(cx, |view, cx| view.open_find(window, cx))))
            .on_action(cx.listener(|this, _: &LinkPullRequest, window, cx| crate::pull_requests::open_link_dialog(&this.link_dialog, window, cx)))
            .on_action(cx.listener(|this, _: &ShowLinkedPullRequests, _, cx| this.state.update(cx, |state, cx| state.add_pull_requests_surface(cx))))
            .on_action(cx.listener(|this, _: &CopyPullRequestNumber, _, cx| {
                if !this.pull_requests.update(cx, |page, cx| page.copy_selected_number(cx)) {
                    cx.propagate();
                }
            }))
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _, cx| {
                this.sidebar.update(cx, |sidebar, cx| sidebar.set_modifiers(event.modifiers, cx));
                this.pull_requests.update(cx, |page, cx| page.set_modifiers(event.modifiers, cx));
            }))
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    let overlay_open = this.menu.read(cx).is_open()
                        || this.dialog.read(cx).is_open()
                        || this.snooze.read(cx).is_open()
                        || this.palette.read(cx).is_open()
                        || crate::updates::overlay(cx).is_some()
                        || crate::voice::is_setup_modal_open(cx)
                        || this.welcome.read(cx).is_open()
                        || crate::settings::overlay(window, cx).is_some()
                        || crate::usage::overlay(window, cx).is_some()
                        || crate::theme_editor::overlay(window, cx).is_some();
                    if this.sidebar.update(cx, |sidebar, cx| sidebar.handle_escape(overlay_open, window, cx)) {
                        cx.stop_propagation();
                    } else if this.state.read(cx).sidebar_sheet.is_open() && !overlay_open {
                        this.state.update(cx, |state, cx| state.close_sidebar_sheet(cx));
                        cx.stop_propagation();
                    }
                } else if !event.keystroke.modifiers.platform
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !on_pull_requests
                    && !composer_replaced
                    && !maximized
                    && (has_thread || has_draft)
                    && let Some(ch) = event.keystroke.key_char.as_deref().filter(|s| !s.is_empty() && !s.chars().all(char::is_whitespace))
                {
                    let overlay_open = this.menu.read(cx).is_open()
                        || this.dialog.read(cx).is_open()
                        || this.snooze.read(cx).is_open()
                        || this.palette.read(cx).is_open()
                        || crate::updates::overlay(cx).is_some()
                        || crate::voice::is_setup_modal_open(cx)
                        || this.welcome.read(cx).is_open()
                        || crate::settings::overlay(window, cx).is_some()
                        || crate::usage::overlay(window, cx).is_some()
                        || crate::theme_editor::overlay(window, cx).is_some();
                    if !overlay_open && this.composer.update(cx, |composer, cx| composer.redirect_printable_key(ch, window, cx)) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &JumpToThread1, _, cx| this.jump(0, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread2, _, cx| this.jump(1, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread3, _, cx| this.jump(2, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread4, _, cx| this.jump(3, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread5, _, cx| this.jump(4, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread6, _, cx| this.jump(5, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread7, _, cx| this.jump(6, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread8, _, cx| this.jump(7, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread9, _, cx| this.jump(8, cx)))
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action({
                let drawer = drawer.clone();
                move |_: &ToggleTerminal, window, cx| drawer.update(cx, |drawer, cx| drawer.toggle(window, cx))
            })
            .on_action(move |action: &crate::worktree_setup::OpenSetupTerminal, window, cx| {
                drawer.update(cx, |drawer, cx| drawer.open_terminal(&action.terminal_id, window, cx))
            })
            .when(sidebar_width > 0., |this| {
                this.child(
                    div()
                        .track_focus(&self.area_focus.sidebar)
                        .w(px(sidebar_width))
                        .h_full()
                        .flex_none()
                        .bg(theme.sidebar)
                        .border_r_1()
                        .border_color(hsla(palette(cx).sidebar_edge))
                        .child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full())),
                )
            })
            .when(on_pull_requests, |this| this.child(div().flex_1().h_full().min_w_0().child(self.pull_requests.clone())))
            .when(!maximized && !on_pull_requests, |this| {
                this.child(
                v_flex()
                    .relative()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .on_scroll_wheel(cx.listener(Self::handle_history_swipe_wheel))
                    .child(self.render_titlebar(window, cx))
                    .when(has_thread, |this| {
                        // The inline details card sits over the canvas; the lane moves left to clear it.
                        this.child(
                            v_flex()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .pr(px(details_layout.lane_inset))
                                // An uncached shell lets minimap frames reuse the transcript's row cache.
                                .child(
                                    div()
                                        .relative()
                                        .flex_1()
                                        .min_h_0()
                                        .child(div().track_focus(&self.area_focus.conversation).size_full().child(self.thread.clone()))
                                        .child(ThreadView::floating_bottom_overlay(
                                            &self.thread,
                                            self.composer_overlay_height.clone(),
                                            v_flex()
                                                .w_full()
                                                .child(self.lineage.clone())
                                                .when(!composer_replaced, |this| {
                                                    this.child(div().track_focus(&self.area_focus.composer).w_full().child(self.composer.clone()))
                                                }),
                                        )),
                                )
                                .child(self.details.clone()),
                        )
                        .child(
                            div()
                                .track_focus(&self.area_focus.terminal)
                                .w_full()
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .child(self.terminal.clone()),
                        )
                    })
                    .when(has_draft, |this| {
                        this.child(
                            v_flex()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .pr(px(details_layout.lane_inset))
                                .child(div().track_focus(&self.area_focus.composer).flex_1().min_h_0().child(self.draft.clone()))
                                .child(self.details.clone()),
                        )
                        .child(
                            div()
                                .track_focus(&self.area_focus.terminal)
                                .w_full()
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .child(self.terminal.clone()),
                        )
                    })
                    .when(!has_thread && !has_draft, |this| {
                        this.child(div().track_focus(&self.area_focus.home).flex_1().min_h_0().child(self.home.clone()))
                    })
                    .when_some(swipe_indicator, |this, indicator| {
                        this.child(crate::quit_hold::swipe_indicator(indicator, pane_height, cx))
                    }),
                )
            })
            .when(!on_pull_requests, |this| this.child(div().track_focus(&self.area_focus.right_panel).h_full().child(self.right_panel.clone())))
            .when(sidebar_width > 0., |this| this.child(self.render_sidebar_rail(sidebar_width, cx)))
            .when_some(sheet, |this, width| this.child(self.render_sidebar_sheet(width, cx)))
            .child(self.render_sidebar_toggle(cx))
            .children(crate::transcript::html_render::fullscreen(window, cx))
            .children(crate::transcript::mcp_app::fullscreen(window, cx))
            .children(crate::media::fullscreen(window, cx))
            .child(self.prompt_links.clone())
            .child(self.menu.clone())
            .child(self.dialog.clone())
            .child(self.guest_share.clone())
            .child(self.git_dialogs.clone())
            .child(self.snooze.clone())
            .child(self.palette.clone())
            .child(self.link_dialog.clone())
            .child(self.device_setup.clone())
            .child(self.ssh_prompt.clone())
            .child(self.welcome.clone())
            .children(crate::voice::render_active_voice_setup_modal(cx))
            .children(crate::toast::stack(&self.state, window, cx))
            .children(crate::github_star_prompt::confetti_overlay(&self.state, cx))
            .children(crate::updates::overlay(cx))
            .children(settings)
            .children(theme_editor)
            .children(usage)
            .children(crate::quit_hold::hint(cx))
            .into_any_element()
    }
}

impl Workspace {
    fn resize_sidebar(&mut self, event: &DragMoveEvent<SidebarResize>, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = f32::from(window.viewport_size().width);
        let current = sidebar_inline_width(self.state.read(cx), viewport);
        let width = layout::drag_width(current, f32::from(event.event.position.x), viewport, BRAND_WIDTH);
        self.rail_dragging = true;
        if width == current {
            return;
        }
        self.state.update(cx, |state, _| state.set_sidebar_width(width, false));
        cx.notify();
        // Persist once the drag settles, not on every frame of it.
        self._save_sidebar_width = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let _ = this.update(cx, |this, cx| this.state.update(cx, |state, _| state.set_sidebar_width(width, true)));
        }));
    }

    /// `SidebarRail`: a 16px hit target centred on the sidebar's right edge, above the thread so
    /// its outer half still takes the pointer. Double-click forgets the stored width.
    fn render_sidebar_rail(&self, sidebar_width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let border = hsla(palette(cx).sidebar_border);
        let dragging = self.rail_dragging && cx.has_active_drag();
        div()
            .id("sidebar-rail")
            .group("sidebar-rail")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(sidebar_width - layout::RAIL_WIDTH / 2.))
            .w(px(layout::RAIL_WIDTH))
            .cursor_ew_resize()
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Drag to resize sidebar").build(window, cx))
            .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| EmptyView))
            .on_drag_move(cx.listener(Self::resize_sidebar))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    this._save_sidebar_width = None;
                    this.state.update(cx, |state, _| state.reset_sidebar_width());
                    cx.notify();
                }
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(layout::RAIL_WIDTH / 2. - 1.))
                    .w(px(2.))
                    .when(dragging, |this| this.bg(border))
                    .group_hover("sidebar-rail", move |this| this.bg(border)),
            )
    }

    /// Below the breakpoint the sidebar is a sheet over a dimmed backdrop that dismisses it.
    fn render_sidebar_sheet(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .id("sidebar-sheet")
            .absolute()
            .inset_0()
            .occlude()
            .child(
                div()
                    .id("sidebar-sheet-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(hsla(p.background).opacity(0.6))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.close_sidebar_sheet(cx)))),
            )
            .child(
                div()
                    .track_focus(&self.area_focus.sidebar)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(width))
                    .bg(hsla(p.sidebar))
                    .border_r_1()
                    .border_color(hsla(p.sidebar_edge))
                    .child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full())),
            )
    }

    /// The workspace control at x = 91 (design-system §5), present whether or not the sidebar shows.
    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let label = keymap::label("sidebar.toggle", cx).map_or_else(|| "Toggle sidebar".to_owned(), |key| format!("Toggle sidebar ({key})"));
        div()
            .id("sidebar-toggle")
            .absolute()
            .left(px(91.))
            .top(px(12.))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.sidebar_accent)))
            .child(Icon::PanelLeft.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.toggle_sidebar_or_sheet(cx))))
    }

    fn traverse(&mut self, next: bool, cx: &mut Context<Self>) {
        if !self.state.read(cx).fleet.environments.is_empty() {
            let target = { let state = self.state.read(cx); let selected = state.selected_thread.as_deref().and_then(|id| state.active_thread_key(id)); state.sidebar.fleet.adjacent_thread(selected.as_ref(), next).and_then(|key| state.sidebar.fleet.thread(&key).map(|row| (row.owner.clone(), key))) };
            if let Some((owner, key)) = target {
                self.state.update(cx, |state, cx| {
                    state.anchor_opened_scoped_thread(&key, cx);
                    state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(key.thread_id), cx);
                });
            }
            return;
        }
        let target = {
            let state = self.state.read(cx);
            state.sidebar.model.adjacent_thread(state.selected_thread.as_deref(), next).map(str::to_owned)
        };
        if let Some(id) = target {
            self.state.update(cx, |state, cx| {
                state.anchor_opened_thread(&id, cx);
                state.select_thread(&id, cx);
            });
        }
    }

    fn jump(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.state.read(cx).fleet.environments.is_empty() {
            let target = { let state = self.state.read(cx); state.sidebar.fleet.jump_target(index).and_then(|key| state.sidebar.fleet.thread(&key).map(|row| (row.owner.clone(), key))) };
            if let Some((owner, key)) = target {
                self.state.update(cx, |state, cx| {
                    state.anchor_opened_scoped_thread(&key, cx);
                    state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(key.thread_id), cx);
                });
            }
            return;
        }
        if let Some(id) = self.state.read(cx).sidebar.model.jump_target(index).map(str::to_owned) {
            self.state.update(cx, |state, cx| {
                state.anchor_opened_thread(&id, cx);
                state.select_thread(&id, cx);
            });
        }
    }

    /// `thread.settle`: settles the open thread and moves on, or un-settles a settled one.
    fn settle_selected(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(id) = state.selected_thread.clone() else { return };
            let Some(thread) = state.sidebar.model.thread(&id) else { return };
            if thread.is_settled() {
                return state.unsettle_thread(&id, cx);
            }
            if !state.fleet.environments.is_empty() {
                let Some(key) = state.active_thread_key(&id) else { return };
                let next = state.sidebar.fleet.adjacent_thread(Some(&key), true).or_else(|| state.sidebar.fleet.adjacent_thread(Some(&key), false)).and_then(|key| state.sidebar.fleet.thread(&key).map(|row| (row.owner.clone(), key)));
                state.run_scoped_thread_action(&key, t3_core::thread_actions::ThreadAction::Settle, &[], cx);
                if let Some((owner, next_key)) = next {
                    state.anchor_opened_scoped_thread(&next_key, cx);
                    state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(next_key.thread_id), cx);
                }
                return;
            }
            let next = state.sidebar.model.adjacent_thread(Some(&id), true).or_else(|| state.sidebar.model.adjacent_thread(Some(&id), false)).map(str::to_owned);
            state.settle_thread(&id, cx);
            if let Some(next) = next {
                state.anchor_opened_thread(&next, cx);
                state.select_thread(&next, cx);
            }
        });
    }

    fn cycle_appearance(&mut self, cx: &mut Context<Self>) {
        use t3_core::client_settings::{AppearanceMode, ClientSettingChange};
        let current = crate::settings::store::client(cx).appearance_mode();
        let next = match current {
            AppearanceMode::System => AppearanceMode::Light,
            AppearanceMode::Light => AppearanceMode::Dark,
            AppearanceMode::Dark => AppearanceMode::System,
        };
        crate::settings::store::update_client(&self.state, ClientSettingChange::AppearanceMode(next), cx);
        let label = match next {
            AppearanceMode::System => "System",
            AppearanceMode::Light => "Light",
            AppearanceMode::Dark => "Dark",
        };
        self.state.update(cx, |state, cx| {
            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Info, format!("Appearance: {label}")), cx);
        });
    }

    fn regenerate_selected_title(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(id) = state.selected_thread.clone() else { return };
            if !state.fleet.environments.is_empty()
                && let Some(key) = state.active_thread_key(&id)
            {
                state.run_scoped_thread_action(&key, t3_core::thread_actions::ThreadAction::RegenerateTitle, &[], cx);
                return;
            }
            state.run_thread_action(&id, t3_core::thread_actions::ThreadAction::RegenerateTitle, &[], cx);
        });
    }

    fn pin_selected(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(id) = state.selected_thread.clone() else { return };
            if !state.fleet.environments.is_empty()
                && let Some(key) = state.active_thread_key(&id)
                && let Some(row) = state.scoped_thread(&key).cloned()
            {
                let action = if row.thread.pinned {
                    t3_core::thread_actions::ThreadAction::Unpin
                } else {
                    t3_core::thread_actions::ThreadAction::Pin
                };
                state.run_scoped_thread_action(&key, action, &[], cx);
                return;
            }
            match state.sidebar.model.thread(&id).map(|thread| thread.pinned) {
                Some(true) => state.request_unpin_thread(&id, cx),
                Some(false) => state.pin_thread(&id, None, cx),
                None => {}
            }
        });
    }

    fn copy_selected_reference(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            #[cfg(feature = "snapshot")]
            let fixture_model = if crate::webview::is_offscreen() && state.selected_thread.is_none()
            {
                std::env::args()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .find(|pair| pair[0] == "--copy-thread-fixture")
                    .map(|pair| {
                        let thread: t3_core::shell::ThreadShell = serde_json::from_slice(
                            &std::fs::read(&pair[1]).expect("copy fixture file"),
                        )
                        .expect("copy fixture thread");
                        assert!(
                            thread.id.starts_with("snapshot-copy-"),
                            "only synthetic copy targets"
                        );
                        state.stop_pump();
                        let original = std::mem::take(&mut state.sidebar.model);
                        let mut model = t3_core::sidebar::SidebarModel::default();
                        model.index_by_id.insert(thread.id.clone(), 0);
                        model
                            .threads
                            .push(t3_core::sidebar::thread::present_thread(&thread, None));
                        state.sidebar.model = std::sync::Arc::new(model);
                        // Assign directly: selecting a thread normally sends a visit to the server.
                        state.selected_thread = Some(thread.id);
                        original
                    })
            } else {
                None
            };
            let Some(id) = state.selected_thread.clone() else {
                return;
            };
            let pr_url = if !state.fleet.environments.is_empty() {
                state
                    .active_thread_key(&id)
                    .and_then(|key| state.scoped_thread(&key))
                    .and_then(|row| t3_core::pull_requests::thread_badge::resolve_thread_current_pull_request_link(&row.thread.pull_requests).map(|link| link.url.clone()))
            } else {
                state
                    .thread_shell(&id)
                    .and_then(|shell| t3_core::pull_requests::thread_badge::resolve_thread_current_pull_request_link(&shell.pull_requests).map(|link| link.url.clone()))
            };
            if let Some(url) = pr_url {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
            } else if !state.fleet.environments.is_empty()
                && let Some(key) = state.active_thread_key(&id)
            {
                state.run_scoped_thread_action(
                    &key,
                    t3_core::thread_actions::ThreadAction::CopyThread,
                    &[],
                    cx,
                );
            } else {
                state.run_thread_action(
                    &id,
                    t3_core::thread_actions::ThreadAction::CopyThread,
                    &[],
                    cx,
                );
            }
            #[cfg(feature = "snapshot")]
            if let Some(model) = fixture_model {
                state.selected_thread = None;
                state.sidebar.model = model;
            }
        });
    }

    /// `startKeybindingsUpdateToasts` (`KeybindingsUpdateToast.tsx`): shows a deduplicated warning
    /// toast on live `keybindingsUpdated` stream events that carry `keybindings.*` config issues.
    fn handle_keybindings_config_toast(&mut self, cx: &mut Context<Self>) {
        let (conn_id, seq, issues) = {
            let state = self.state.read(cx);
            (
                state.connection_id(),
                state.server_config.keybindings_updated_seq(),
                state.server_config.config.as_ref().map(|config| config.issues.clone()).unwrap_or_default(),
            )
        };
        if conn_id != self.seen_keybindings_connection {
            self.seen_keybindings_connection = conn_id;
            self.seen_keybindings_updated_seq = seq;
            self.keybindings_toast.reset();
            return;
        }
        if seq == self.seen_keybindings_updated_seq {
            return;
        }
        self.seen_keybindings_updated_seq = seq;
        if let Some(KeybindingsUpdateToastDecision::InvalidConfiguration(message)) =
            self.keybindings_toast.handle(&issues, crate::sidebar::controller::now_ms())
        {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    crate::toast::Toast::new(crate::toast::ToastKind::Warning, "Invalid keybindings configuration")
                        .description(message)
                        .open_keybindings_config(),
                    cx,
                );
            });
        }
    }

    /// `HostFreezeToastCoordinator.ts`: tells the user once per reported stall why a host stopped
    /// answering when it reports `lastEventLoopStall` on connect/reconnect.
    fn handle_host_freeze_toasts(&mut self, cx: &mut Context<Self>) {
        let notices: Vec<(String, String, String, u64, Option<u64>, u32)> = {
            let state = self.state.read(cx);
            let mut out = Vec::new();
            if let Some(client) = state.client_handle()
                && let Some(stall) = client.server_config_extras().borrow().last_event_loop_stall.clone()
            {
                let env_key = state
                    .active_environment()
                    .and_then(|e| e.environment_id.clone().or_else(|| Some(e.id.clone())))
                    .unwrap_or_else(|| "local".to_owned());
                let label = state
                    .active_environment()
                    .map(|e| e.label.clone())
                    .or_else(|| state.server_config.config.as_ref().map(|c| c.environment.label.clone()))
                    .unwrap_or_default();
                if let Some(notice) = host_freeze_notice(&label, Some(&stall)) {
                    out.push((
                        env_key,
                        notice.title,
                        notice.description,
                        stall.duration_ms.to_bits(),
                        stall.load_average.map(f64::to_bits),
                        stall.cpu_count,
                    ));
                }
            }
            out
        };
        for (env_key, title, description, dur, load, cpus) in notices {
            if self.shown_host_freezes.insert((env_key, dur, load, cpus)) {
                self.state.update(cx, |state, cx| {
                    state.show_toast(
                        crate::toast::Toast::new(crate::toast::ToastKind::Warning, title).description(description),
                        cx,
                    );
                });
            }
        }
    }

    /// `PermissionUpdateNotice.ts`: asks once per environment (until dismissed) to re-pair a
    /// connection that still uses pre-scope legacy permissions, offering `"Open Sharing & access"`.
    fn handle_permission_update_toasts(&mut self, cx: &mut Context<Self>) {
        let (candidates, upgraded): (Vec<(String, String)>, Vec<String>) = {
            let state = self.state.read(cx);
            let mut out = Vec::new();
            let mut upgraded = Vec::new();
            let mut seen = HashSet::new();
            for env in &state.fleet.environments {
                let Some(session) = env.grants.as_ref() else { continue };
                let env_key = env
                    .config
                    .config
                    .as_ref()
                    .map(|c| c.environment.environment_id.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(env.summary.owner.saved_id.as_str())
                    .to_owned();
                if !session_has_legacy_permissions(session) {
                    if self.shown_permission_notices.contains(&env_key) {
                        upgraded.push(env_key);
                    }
                    continue;
                }
                if seen.insert(env_key.clone()) {
                    let label = state
                        .environments
                        .get(&env.summary.owner.saved_id)
                        .map(|e| e.label.clone())
                        .or_else(|| env.config.config.as_ref().map(|c| c.environment.label.clone()))
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| "this machine".to_owned());
                    out.push((env_key, label));
                }
            }
            if let Some(env) = state.active_environment()
                && let Some(session) = state.client_handle().and_then(|client| client.session_state().borrow().clone())
            {
                let env_key = env.environment_id.as_deref().unwrap_or(env.id.as_str()).to_owned();
                if session_has_legacy_permissions(&session) {
                    if seen.insert(env_key.clone()) {
                        out.push((env_key, env.label.clone()));
                    }
                } else if self.shown_permission_notices.contains(&env_key) {
                    upgraded.push(env_key);
                }
            }
            (out, upgraded)
        };
        if !upgraded.is_empty() {
            for env_key in &upgraded {
                self.shown_permission_notices.remove(env_key);
            }
            self.state.update(cx, |state, cx| {
                for env_key in upgraded {
                    state.close_toast_by_key(&format!("permission-update:{env_key}"), cx);
                }
            });
        }
        for (env_key, label) in candidates {
            if self.shown_permission_notices.contains(&env_key) {
                continue;
            }
            let ui_key = format!("permission-update-dismissed-{env_key}");
            if t3_client::store::load_ui_state(&ui_key).and_then(|v| v.as_bool()) == Some(true) {
                self.shown_permission_notices.insert(env_key);
                continue;
            }
            self.shown_permission_notices.insert(env_key.clone());
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    crate::toast::Toast::new(
                        crate::toast::ToastKind::Warning,
                        format!("Permissions have changed for {label}"),
                    )
                    .description("This connection still uses the old permissions, so some actions may no longer be available. Pair again using a new link with the permissions you need.")
                    .permission_update(env_key),
                    cx,
                );
            });
        }
    }

    /// `HistorySwipeNavigation.tsx` (`AP-SWIPE`): macOS two-finger horizontal trackpad swipes go
    /// back and forward in thread navigation history.
    fn handle_history_swipe_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let blocked = self.has_blocking_overlay(window, cx);
        let (is_pixel_delta, delta_x, delta_y) = match event.delta {
            // GPUI's pixel delta has the opposite sign of DOM WheelEvent.deltaX/Y.
            ScrollDelta::Pixels(p) => (true, -f32::from(p.x), -f32::from(p.y)),
            ScrollDelta::Lines(l) => (false, -l.x, -l.y),
        };
        let phase = match event.touch_phase {
            TouchPhase::Started => SwipePhase::Started,
            TouchPhase::Moved => SwipePhase::Moved,
            TouchPhase::Ended | TouchPhase::Cancelled => SwipePhase::Ended,
        };
        let wheel = SwipeWheel {
            delta_x,
            delta_y,
            y: f32::from(event.position.y),
            phase,
            is_pixel_delta,
            ctrl_key: event.modifiers.control,
            blocked,
        };
        let before = self.history_swipe.indicator();
        let action = {
            let state = self.state.read(cx);
            self.history_swipe.on_wheel(wheel, |dir| state.can_navigate(dir.is_back()))
        };
        if let SwipeAction::Navigate(dir) = action {
            self.state.update(cx, |state, cx| state.navigate_history(dir.is_back(), cx));
        }
        let after = self.history_swipe.indicator();
        if after.is_some_and(|i| !i.tracking) && self._swipe_clear.is_none() {
            self._swipe_clear = Some(cx.spawn(async move |this, cx| {
                for opacity in [0.75_f32, 0.5, 0.25, 0.0] {
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    let keep_going = this.update(cx, |ws, cx| {
                        ws.history_swipe.set_indicator_opacity(opacity);
                        cx.notify();
                        ws.history_swipe.indicator().is_some_and(|i| !i.tracking)
                    });
                    if !matches!(keep_going, Ok(true)) {
                        return;
                    }
                }
                let _ = this.update(cx, |ws, cx| {
                    ws.history_swipe.clear_indicator();
                    ws._swipe_clear = None;
                    cx.notify();
                });
            }));
        } else if after.is_some_and(|i| i.tracking) {
            self._swipe_clear = None;
        }
        if before != after {
            cx.notify();
        }
    }

    /// `startDocumentTitle` (`appInit.tsx`, `pingdotgg/t3code#34697`): keeps the native window
    /// title synced with the open thread/project, leading with status + thread name when
    /// `screenReaderWindowTitle` is enabled.
    fn sync_window_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sr_title = t3_core::announcements::screen_reader_window_title_setting(
            crate::settings::client_settings(cx).entries(),
        );
        let state = self.state.read(cx);
        let thread = state
            .selected_thread
            .as_deref()
            .and_then(|id| state.sidebar.model.thread(id));
        let project_title = thread
            .and_then(|t| state.shell.project(&t.project_id))
            .or_else(|| state.draft_project.as_deref().and_then(|id| state.shell.project(id)))
            .map(|p| p.title.as_str());
        let thread_title = thread.map(|t| t.title.as_str());
        let screen_reader_status = sr_title.then(|| {
            thread.and_then(t3_core::announcements::resolve_window_title_thread_status)
        });
        let next = t3_core::announcements::resolve_window_title(
            "T3 Code",
            project_title,
            thread_title,
            screen_reader_status,
            true,
        );
        if self.last_window_title.as_deref() != Some(next.as_str()) {
            window.set_window_title(&next);
            self.last_window_title = Some(next);
        }
    }

    fn has_blocking_overlay(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        // Note: `updates::overlay(cx)` returns the persistent `UpdateDialog` entity whenever
        // `UpdateControl` is mounted (even while closed), so it cannot be checked with `.is_some()`.
        self.menu.read(cx).is_open()
            || self.dialog.read(cx).is_open()
            || self.snooze.read(cx).is_open()
            || self.palette.read(cx).is_open()
            || crate::voice::is_setup_modal_open(cx)
            || self.welcome.read(cx).is_open()
            || crate::settings::overlay(window, cx).is_some()
            || crate::usage::overlay(window, cx).is_some()
            || crate::theme_editor::overlay(window, cx).is_some()
    }

    /// `workspace.focusNext` / `workspace.focusPrevious` (`workspaceFocus.ts`): cycles focus
    /// between visible top-level workspace regions without escaping active modals or overlays.
    fn move_workspace_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_blocking_overlay(window, cx) {
            return;
        }
        let state = self.state.read(cx);
        let viewport = f32::from(window.viewport_size().width);
        let sidebar_visible = sidebar_inline_width(state, viewport) > 0. || state.sidebar_sheet.is_open();
        let on_pull_requests = state.pull_requests_open;
        let has_thread = state.selected_thread.is_some();
        let has_draft = !has_thread && state.draft_project.is_some();
        let panel = state.panel_thread().as_deref().map(|thread| state.panels.panels.panel(thread));
        let right_panel_open = panel.is_some_and(|panel| panel.is_open);
        let maximized = panel.is_some_and(|panel| panel.is_open && panel.maximized);
        let terminal_open = self.terminal.read(cx).is_open(cx);

        let mut visible = Vec::new();
        if sidebar_visible {
            visible.push(WorkspaceFocusArea::Sidebar);
        }
        if on_pull_requests {
            visible.push(WorkspaceFocusArea::PullRequests);
        } else if !maximized {
            if has_thread {
                visible.push(WorkspaceFocusArea::Conversation);
                if !self.composer_replaced {
                    visible.push(WorkspaceFocusArea::Composer);
                }
            } else if has_draft {
                visible.push(WorkspaceFocusArea::Composer);
            } else {
                visible.push(WorkspaceFocusArea::Home);
            }
            if (has_thread || has_draft) && terminal_open {
                visible.push(WorkspaceFocusArea::Terminal);
            }
        }
        if !on_pull_requests && right_panel_open {
            visible.push(WorkspaceFocusArea::RightPanel);
        }

        let current = visible.iter().copied().find(|area| match area {
            WorkspaceFocusArea::Sidebar => self.area_focus.sidebar.contains_focused(window, cx),
            WorkspaceFocusArea::Conversation => self.area_focus.conversation.contains_focused(window, cx),
            WorkspaceFocusArea::Composer => self.area_focus.composer.contains_focused(window, cx),
            WorkspaceFocusArea::Terminal => self.area_focus.terminal.contains_focused(window, cx),
            WorkspaceFocusArea::RightPanel => self.area_focus.right_panel.contains_focused(window, cx),
            WorkspaceFocusArea::PullRequests => self.pull_requests.read(cx).focus_handle(cx).contains_focused(window, cx),
            WorkspaceFocusArea::Home => self.area_focus.home.contains_focused(window, cx),
        });
        let Some(next) = next_workspace_focus_area(&visible, current.or(self.area_focus.last_area), forward) else {
            return;
        };
        self.area_focus.last_area = Some(next);
        match next {
            WorkspaceFocusArea::Sidebar => window.focus(&self.area_focus.sidebar, cx),
            WorkspaceFocusArea::Conversation => window.focus(&self.area_focus.conversation, cx),
            WorkspaceFocusArea::Composer => self.composer.update(cx, |composer, cx| composer.focus(window, cx)),
            WorkspaceFocusArea::Terminal => window.focus(&self.area_focus.terminal, cx),
            WorkspaceFocusArea::RightPanel => window.focus(&self.area_focus.right_panel, cx),
            WorkspaceFocusArea::PullRequests => {
                let handle = self.pull_requests.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            WorkspaceFocusArea::Home => window.focus(&self.area_focus.home, cx),
        }
    }

    /// Whether the main composer is currently shown and unobstructed by any modal/overlay.
    fn composer_visible(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        if self.composer_replaced || self.has_blocking_overlay(window, cx) {
            return false;
        }
        let state = self.state.read(cx);
        if state.pull_requests_open || (state.selected_thread.is_none() && state.draft_project.is_none()) {
            return false;
        }
        let maximized = state
            .panel_thread()
            .as_deref()
            .map(|thread| state.panels.panels.panel(thread))
            .is_some_and(|panel| panel.is_open && panel.maximized);
        !maximized
    }

    /// `composer.branch`: opens the composer's base branch picker only while the composer is visible.
    fn open_composer_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_visible(window, cx) {
            return;
        }
        self.composer.update(cx, |composer, cx| composer.open_base_branch_picker(window, cx));
    }

    /// `composer.increaseEffort` / `composer.decreaseEffort`: steps reasoning effort only while
    /// the composer is visible.
    fn step_composer_effort(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_visible(window, cx) {
            return;
        }
        self.composer.update(cx, |composer, cx| composer.step_reasoning_effort(direction, window, cx));
    }

    /// `composer.sendBackground`: starts a draft thread in the background and keeps the draft open.
    fn send_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_visible(window, cx) {
            return;
        }
        let state = self.state.read(cx);
        if state.selected_thread.is_none() && state.draft_project.is_some() {
            self.composer.update(cx, |composer, cx| {
                composer.send_background(window, cx);
            });
        }
    }

    /// `composer.sendAndNewThread`: sends to the active server thread and opens a fresh draft in
    /// the thread's project only after the send succeeds (`SendAndNewThreadCompleted`).
    fn send_and_new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_visible(window, cx) {
            return;
        }
        if self.state.read(cx).selected_thread.is_some() {
            self.composer.update(cx, |composer, cx| {
                composer.send_and_new_thread(window, cx);
            });
        }
    }

    fn finish_send_and_new_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let project_id = {
            let state = self.state.read(cx);
            if state.selected_thread.as_deref() != Some(thread_id) {
                return;
            }
            state
                .thread_shell(thread_id)
                .map(|thread| thread.project_id.clone())
                .or_else(|| new_thread::default_project(state))
        };
        let Some(project_id) = project_id else {
            return;
        };
        self.state.update(cx, |state, cx| state.start_draft(&project_id, cx));
        let composer = self.composer.clone();
        window.defer(cx, move |window, cx| {
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        });
    }

    /// `rightPanel.new`: opens the right panel's add-surface menu while the panel is open and no
    /// blocking overlay is shown (`RightPanelTabs.tsx`).
    fn open_right_panel_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_blocking_overlay(window, cx) {
            return;
        }
        self.right_panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[gpui::test]
    async fn workspace_wires_keybindings_toast_action_and_send_and_new_thread_handoff(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::composer::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::init(installed.0.clone(), cx);
            crate::theme_editor::init(installed.0.clone(), cx);
            crate::usage::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            if let Some(config) = state.server_config.config.as_mut() {
                config.keybindings_config_path = Some("/tmp/keybindings.json".to_owned());
                config.available_editors = vec!["vscode".to_owned()];
                config.providers = serde_json::from_value(json!([{
                    "driver": "claudeAgent",
                    "instanceId": "claudeAgent",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated" },
                    "models": [{ "slug": "sonnet", "name": "Sonnet", "isCustom": false }]
                }]))
                .unwrap();
            }
            state.select_thread("menu-thread", cx);
        });

        let window = cx.open_window(gpui::size(px(1200.), px(800.)), |_, _| gpui::EmptyView);
        let workspace = cx
            .update_window(window.into(), |_, window, cx| cx.new(|cx| Workspace::new(state.clone(), window, cx)))
            .unwrap();

        // 1. Live `keybindingsUpdated` stream issue -> warning toast with inline `Open keybindings.json` enabled.
        state.update(cx, |state, cx| {
            let event: t3_core::server_config::ServerConfigStreamEvent = serde_json::from_value(json!({
                "version": 1,
                "type": "keybindingsUpdated",
                "payload": {
                    "keybindings": [],
                    "issues": [{ "kind": "keybindings.invalid-entry", "message": "Invalid chord on line 2", "index": 0 }]
                }
            }))
            .unwrap();
            assert!(state.server_config.apply(event));
            cx.emit(AppEvent::ServerConfig);
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let toast = state.toasts.latest().expect("keybindings warning toast");
            assert_eq!(toast.kind, crate::toast::ToastKind::Warning);
            assert_eq!(toast.title, "Invalid keybindings configuration");
            assert_eq!(toast.description.as_deref(), Some("Invalid chord on line 2"));
            assert!(toast.open_keybindings_config, "warning toast must carry Open keybindings.json action");
        });

        let expected_project = state.read_with(cx, |state, _| {
            state.thread_shell("menu-thread").expect("fixture thread").project_id.clone()
        });
        // 2. `composer.sendAndNewThread` sends to `menu-thread` and switches to a fresh draft ONLY after RPC succeeds.
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                assert!(ws.composer_visible(window, cx), "composer must be visible when no overlay is open");
                ws.composer.update(cx, |composer, cx| composer.set_prompt("Ship the follow-up", window, cx));
                ws.send_and_new_thread(window, cx);
            });
        })
        .unwrap();
        // While RPC is in flight, the workspace stays on `menu-thread` (no premature draft switch).
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_thread.as_deref(), Some("menu-thread"));
            assert!(state.draft_project.is_none());
        });
        let (method, payload, reply) = loop {
            let (method, payload, reply) = fixture.next_request().await;
            if method == t3_core::commands::method::DISPATCH_COMMAND && payload["type"] == "message.dispatch" {
                break (method, payload, reply);
            }
            let _ = reply.send(Ok(json!({})));
        };
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(payload["threadId"], "menu-thread");
        let (draft_tx, draft_rx) = tokio::sync::oneshot::channel::<()>();
        let mut draft_tx = Some(draft_tx);
        let expected_for_sub = expected_project.clone();
        let _draft_sub = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event: &AppEvent, _| {
                if *event == AppEvent::Selection
                    && state.selected_thread.is_none()
                    && state.draft_project.as_deref() == Some(expected_for_sub.as_str())
                    && let Some(tx) = draft_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply.send(Ok(json!({}))).unwrap();
        draft_rx.await.unwrap();
        drop(_draft_sub);
        cx.run_until_parked();

        // After RPC succeeds, workspace switches to a fresh draft in the thread's project.
        state.read_with(cx, |state, _| {
            assert!(state.selected_thread.is_none(), "sendAndNewThread opens a fresh draft after success");
            assert_eq!(state.draft_project.as_deref(), Some(expected_project.as_str()));
        });

        // 3. Blocking overlays, Pull Requests page, maximized right panel, and `composer_replaced`
        // guard workspace-level composer & right-panel shortcuts (`NewSurface`, `OpenBranch`,
        // `IncreaseEffort`, `DecreaseEffort`, `SendAndNewThread`).
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
            state.toggle_right_panel(cx);
        });
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                assert!(ws.composer_visible(window, cx));

                // Opening CommandPalette blocks both `open_right_panel_add_menu` and composer actions.
                ws.palette.update(cx, |palette, cx| palette.toggle(window, cx));
                assert!(ws.has_blocking_overlay(window, cx));
                assert!(!ws.composer_visible(window, cx));
                ws.open_right_panel_add_menu(window, cx);
                assert!(!ws.right_panel.read(cx).is_add_menu_open(), "overlay must block rightPanel.new");
                ws.palette.update(cx, |palette, cx| palette.toggle(window, cx));
                assert!(!ws.has_blocking_overlay(window, cx));

                // With overlay closed, `open_right_panel_add_menu` opens the add-surface menu.
                ws.open_right_panel_add_menu(window, cx);
                assert!(ws.right_panel.read(cx).is_add_menu_open());
                ws.right_panel.update(cx, |panel, cx| panel.close_add_surface_menu(cx));

                // Pull Requests page hides composer.
                ws.state.update(cx, |state, _| state.pull_requests_open = true);
                assert!(!ws.composer_visible(window, cx));
                ws.state.update(cx, |state, _| state.pull_requests_open = false);

                // Maximized right panel hides composer.
                ws.state.update(cx, |state, cx| state.toggle_right_panel_maximized(cx));
                assert!(!ws.composer_visible(window, cx));
                ws.state.update(cx, |state, cx| state.toggle_right_panel_maximized(cx));
                assert!(ws.composer_visible(window, cx));

                // `composer_replaced` hides composer.
                ws.composer_replaced = true;
                assert!(!ws.composer_visible(window, cx));
                ws.composer_replaced = false;

                // 4. `sync_window_title` tracks active thread & project title.
                ws.sync_window_title(window, cx);
                assert!(
                    ws.last_window_title.as_deref().is_some_and(|t| t.contains('/')),
                    "desktop window title should format `project / thread`, got {:?}",
                    ws.last_window_title
                );
            });
        })
        .unwrap();

        // 5. Legacy-permissions session triggers a deduplicated persistent permission-update toast.
        state.update(cx, |state, _| {
            state.environments.environments.push(t3_client::store::SavedEnvironment {
                id: "legacy-saved".into(),
                environment_id: Some("legacy-env-id".into()),
                label: "Studio".into(),
                http_base: "http://127.0.0.1:3773".into(),
                access_token: Some("tok".into()),
                local: true,
                disabled: false,
                ssh: None,
                server_version: None,
            });
            state.environments.active = Some("legacy-saved".into());
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "role": "client",
                    "permissions": ["orchestration:read", "orchestration:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |_, cx| cx.emit(AppEvent::Phase));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let latest = state.toasts.latest().expect("permission update toast");
            assert_eq!(latest.kind, crate::toast::ToastKind::Warning);
            assert_eq!(latest.title, "Permissions have changed for Studio");
            assert_eq!(latest.permission_update_env.as_deref(), Some("legacy-env-id"));
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
