//! Window root: titlebar strip, sidebar, and the thread area, or the connect screen. The sidebar
//! resize rail and narrow-window sheet port `components/AppSidebarLayout.tsx` and `SidebarRail`
//! in `components/ui/sidebar.tsx`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{
    ClickEvent, ClipboardItem, Context, DragMoveEvent, EmptyView, Entity, FocusHandle, Focusable, Pixels,
    FontWeight, IntoElement, MouseButton, ParentElement, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription, Task, TouchPhase, Window,
    div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::status::{HopResult, connection_diagnosis_text, diagnose, host_freeze_notice};
use t3_client::supervisor::ConnectionStatus;
use t3_core::keybindings::{WorkspaceFocusArea, next_workspace_focus_area};
use t3_core::pairing::session_has_legacy_permissions;
use t3_core::settings::keybindings::{KeybindingsUpdateToastController, KeybindingsUpdateToastDecision};
use t3_core::settings::quit_hold::swipe::{HistorySwipe, SwipeAction, SwipePhase, SwipeWheel};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::chat_header::{self, ChatHeader};
use crate::composer::{Composer, FocusPrompt, OpenPermissions, ToggleModelPicker};
use crate::connect::ConnectView;
use crate::components::motion::{prefers_reduced_motion, transition};
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
use crate::issues::{CopyIssueKey, IssuesPage};
use crate::pull_requests::{CopyPullRequestNumber, LinkPullRequest, LinkPullRequestDialog, PullRequestsPage, ShowLinkedPullRequests};
use crate::snooze_dialog::SnoozeDialog;
use crate::sidebar::SidebarPane;
use crate::terminal::{self, TerminalDrawer, ToggleTerminal};
use crate::theme::{
    ThemeSlot, ThemeSlotHoverExt as _, hsla, palette,
    theme_slot_background_layer,
};
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

fn sidebar_inline_width_when_open(state: &AppState, viewport: f32) -> f32 {
    layout::inline_width(state.sidebar_width, false, viewport, BRAND_WIDTH)
}

struct SidebarResize;

const SIDEBAR_TOGGLE_SIZE: f32 = 28.;
const SIDEBAR_TOGGLE_GAP: f32 = 12.;

fn native_window_controls_width(window: &Window) -> f32 {
    if cfg!(target_os = "macos") && !window.is_fullscreen() { 90. } else { 0. }
}

fn sidebar_toggle_left(window: &Window) -> Pixels {
    px((native_window_controls_width(window) + 1.).max(SIDEBAR_TOGGLE_GAP))
}

pub(crate) fn traffic_light_inset(window: &Window, collapsed: bool) -> Pixels {
    if collapsed {
        px(native_window_controls_width(window).max(SIDEBAR_TOGGLE_GAP) + SIDEBAR_TOGGLE_SIZE + SIDEBAR_TOGGLE_GAP)
    } else {
        px(20.)
    }
}

pub(crate) fn panel_animation_duration(cx: &gpui::App) -> Duration {
    if prefers_reduced_motion(cx) {
        return Duration::ZERO;
    }
    Duration::from_millis(u64::from(crate::settings::store::try_client(cx).map_or(
        t3_core::client_settings::DEFAULT_PANEL_ANIMATION_DURATION_MS,
        |doc| doc.panel_animation_duration_ms(),
    )))
}

pub(crate) fn panel_width_clip(
    visible: Pixels,
    full: Pixels,
    align_right: bool,
    content: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .h_full()
        .w(visible)
        .overflow_hidden()
        .when(align_right, |clip| clip.justify_end())
        .child(div().flex_none().h_full().w(full).child(content))
}

fn workspace_for_window(window: &Window, cx: &gpui::App) -> Option<Entity<Workspace>> {
    window.root::<Workspace>().flatten().or_else(|| {
        window.root::<gpui_component::Root>().flatten()?.read(cx).view().clone().downcast::<Workspace>().ok()
    })
}

pub(crate) fn measure_draft_composer(composer: impl IntoElement, window: &Window, cx: &gpui::App) -> gpui::Div {
    let measurement = workspace_for_window(window, cx).map(|workspace| {
        let workspace = workspace.read(cx);
        (workspace.draft_composer_height.clone(), workspace.terminal.entity_id())
    });
    div().relative().w_full().child(composer).when_some(measurement, |container, (height, drawer)| {
        container.child(
            gpui::canvas(
                move |bounds, window, _| {
                    let next = f32::from(bounds.size.height).ceil();
                    if height.replace(next) != next {
                        window.on_next_frame(move |_, cx| cx.notify(drawer));
                    }
                },
                |_, _, _, _| {},
            ).absolute().inset_0(),
        )
    })
}

pub(crate) fn terminal_drawer_geometry(window: &Window, cx: &gpui::App) -> (Option<Pixels>, f32) {
    let viewport = window.viewport_size();
    let workspace = workspace_for_window(window, cx);
    let Some(workspace) = workspace else {
        return (None, f32::INFINITY);
    };
    let workspace = workspace.read(cx);
    let state = workspace.state.read(cx);
    let width = f32::from(viewport.width);
    let right = if t3_core::right_panel::is_sheet_mode(width) {
        0.
    } else {
        state.panel_thread().filter(|thread| state.panels.panels.is_open(thread))
            .map_or(0., |thread| crate::right_panel::inline_panel_width(state, &thread, width))
    };
    let center = px((width - sidebar_inline_width(state, width) - right).max(0.));
    let composer = if state.selected_thread.is_some() {
        workspace.composer_overlay_height.get()
    } else {
        workspace.draft_composer_height.get()
    };
    (Some(center), (f32::from(viewport.height) - TITLEBAR_HEIGHT - composer).max(0.))
}

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
    shown_host_freezes: HashSet<String>,
    shown_permission_notices: HashSet<String>,
    connection_diagnosis_open: bool,
    inactive_since: Option<Instant>,
    history_swipe: HistorySwipe,
    last_window_title: Option<String>,
    sidebar: Entity<SidebarPane>,
    thread: Entity<ThreadView>,
    composer: Entity<Composer>,
    lineage: Entity<crate::lineage::ComposerLineage>,
    composer_overlay_height: std::rc::Rc<std::cell::Cell<f32>>,
    draft_composer_height: std::rc::Rc<std::cell::Cell<f32>>,
    composer_replaced: bool,
    draft: Entity<DraftHero>,
    prompt_links: Entity<crate::prompt_links::PromptLinks>,
    home: Entity<crate::home::HomeView>,
    connect: Entity<ConnectView>,
    welcome: Entity<crate::onboarding::WelcomeWizard>,
    menu: Entity<ContextMenu>,
    dialog: Entity<ConfirmDialog>,
    guest_share: Entity<crate::guest_share::GuestShareDialog>,
    mcp: Entity<crate::mcp::McpDialogs>,
    palette: Entity<CommandPalette>,
    snooze: Entity<SnoozeDialog>,
    right_panel: Entity<RightPanelView>,
    terminal: Entity<TerminalDrawer>,
    header: Entity<ChatHeader>,
    details: Entity<DetailsView>,
    git_dialogs: Entity<GitDialogs>,
    pull_requests: Entity<PullRequestsPage>,
    issues: Entity<IssuesPage>,
    link_dialog: Entity<LinkPullRequestDialog>,
    project_dialogs: Entity<crate::projects::ProjectDialogs>,
    device_setup: Entity<crate::device::DeviceSetupDialog>,
    ssh_prompt: Entity<crate::ssh_prompt::SshPromptDialog>,
    snapshot_coordinator: Entity<crate::snapshot_capture::SnapShotCoordinator>,
    snapshot_setup: Entity<crate::snapshot_capture::SnapShotSetupDialog>,
    /// A rail drag is live; `has_active_drag` alone also matches row and tab drags.
    rail_dragging: bool,
    sidebar_is_sheet: Option<bool>,
    theme_background: t3_core::themes::background::ThemeBackgroundTracker,
    theme_background_task: Option<Task<()>>,
    _save_sidebar_width: Option<Task<()>>,
    _slow_rpc_task: Task<()>,
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
        // Subscribe before PromptLinks so `/approve-agent` and `/connect-agent` URLs route to MCP approval.
        let mcp = cx.new(|cx| crate::mcp::McpDialogs::new(state.clone(), window, cx));
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
        let issues = cx.new(|cx| IssuesPage::new(state.clone(), window, cx));
        let link_dialog = cx.new(|cx| LinkPullRequestDialog::new(state.clone(), window, cx));
        let project_dialogs = cx.new(|cx| crate::projects::ProjectDialogs::new(state.clone(), window, cx).with_composer(&composer));
        let device_setup = cx.new(|cx| crate::device::DeviceSetupDialog::new(state.clone(), cx));
        let ssh_prompt = cx.new(|cx| crate::ssh_prompt::SshPromptDialog::new(state.clone(), window, cx));
        let snapshot_coordinator = cx.new(|cx| {
            crate::snapshot_capture::SnapShotCoordinator::new(
                state.clone(),
                composer.clone(),
                window,
                cx,
            )
        });
        let snapshot_setup = cx.new(|cx| {
            crate::snapshot_capture::SnapShotSetupDialog::new(
                state.clone(),
                snapshot_coordinator.clone(),
                window,
                cx,
            )
        });
        crate::snapshot_capture::register_workspace_handles(
            snapshot_coordinator.clone(),
            snapshot_setup.clone(),
            cx,
        );
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
                if matches!(event, AppEvent::Phase)
                    && (matches!(state.read(cx).phase, Phase::Online(ConnectionStatus::Connected))
                        || !matches!(state.read(cx).phase, Phase::Online(_)))
                {
                    this.connection_diagnosis_open = false;
                }
                if matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                    this.handle_keybindings_config_toast(cx);
                }
                if matches!(event, AppEvent::ServerConfig | AppEvent::Phase | AppEvent::Shell) {
                    this.handle_host_freeze_toasts(cx);
                    this.handle_permission_update_toasts(cx);
                    this.sync_theme_background(cx);
                    if matches!(event, AppEvent::ServerConfig) {
                        state.read(cx).sync_learned_environment_routes();
                    }
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
            cx.observe(&snapshot_coordinator, |_, _, cx| cx.notify()),
            cx.observe(&snapshot_setup, |_, _, cx| cx.notify()),
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
            // Coming back to the app retires its delivered notifications, signals connection
            // supervisors (`wakeups.ts`: `application-focused` / `application-active-long`), and
            // relists T3 Connect account environments when signed in (`shouldRelistAccountAfterWakeup`).
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    let wakeup = match this.inactive_since.take() {
                        Some(since) => t3_client::wakeups::classify_app_resume(since.elapsed()),
                        None => t3_client::wakeups::ConnectionWakeup::ApplicationFocused,
                    };
                    this.state.update(cx, |state, cx| {
                        state.clear_pending_notifications(cx);
                        if t3_client::wakeups::should_relist_account_after_wakeup(wakeup)
                            && state.cloud.identity.is_some()
                        {
                            state.refresh_cloud(cx);
                        }
                    });
                    t3_client::wakeups::emit_wakeup(wakeup);
                    this.sync_theme_background(cx);
                    this.snapshot_coordinator.update(cx, |coordinator, cx| {
                        coordinator.refresh_state(cx);
                    });
                } else {
                    this.inactive_since = Some(Instant::now());
                    crate::voice::handle_window_blur(cx);
                }
            }),
        ];
        #[cfg(all(target_os = "macos", not(test)))]
        register_macos_wake_observer();
        let mut slow_rpc_rx = t3_client::session::subscribe_slow_rpc_requests();
        let initial_slow_rpc = slow_rpc_rx.borrow_and_update().clone();
        if !initial_slow_rpc.is_empty() {
            state.update(cx, |state, cx| state.sync_slow_rpc_toast(&initial_slow_rpc, cx));
        }
        let slow_rpc_task = cx.spawn(async move |this, cx| {
            while slow_rpc_rx.changed().await.is_ok() {
                let requests = slow_rpc_rx.borrow_and_update().clone();
                if this
                    .update(cx, |ws, cx| {
                        ws.state.update(cx, |state, cx| state.sync_slow_rpc_toast(&requests, cx));
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
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
            connection_diagnosis_open: false,
            inactive_since: None,
            history_swipe: HistorySwipe::default(),
            last_window_title: None,
            sidebar,
            thread,
            composer,
            lineage,
            composer_overlay_height: std::rc::Rc::new(std::cell::Cell::new(0.)),
            draft_composer_height: std::rc::Rc::new(std::cell::Cell::new(0.)),
            composer_replaced: false,
            draft,
            prompt_links,
            home,
            connect,
            welcome,
            menu,
            dialog,
            guest_share,
            mcp,
            palette,
            snooze,
            right_panel,
            terminal,
            header,
            details,
            git_dialogs,
            pull_requests,
            issues,
            link_dialog,
            project_dialogs,
            device_setup,
            ssh_prompt,
            snapshot_coordinator,
            snapshot_setup,
            rail_dragging: false,
            sidebar_is_sheet: None,
            theme_background: t3_core::themes::background::ThemeBackgroundTracker::new(),
            theme_background_task: None,
            _save_sidebar_width: None,
            _slow_rpc_task: slow_rpc_task,
            _subscriptions: subscriptions,
        };
        workspace.handle_host_freeze_toasts(cx);
        workspace.handle_permission_update_toasts(cx);
        workspace.sync_theme_background(cx);
        workspace.sync_window_title(window, cx);
        workspace
    }

    /// CN-STATUS: while not connected, a clickable status dot and title with the diagnosis
    /// popover (and tooltip while closed), plus Retry where it can do something.
    fn render_connection_status(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let state = self.state.read(cx);
        let Phase::Online(status) = &state.phase else { return None };
        if *status == ConnectionStatus::Connected {
            return None;
        }
        let host = state.active_environment().map(|environment| environment.label.clone()).unwrap_or_default();
        let has_diagnosis = diagnose(status, &host).is_some();
        let details: gpui::SharedString = status.details(&host).into();
        let error_tone = status.summary().1;
        let open = self.connection_diagnosis_open && has_diagnosis;
        let p = palette(cx);
        Some(
            h_flex()
                .flex_none()
                .gap(px(6.))
                .child(
                    h_flex()
                        .id("connection-status")
                        .gap(px(4.))
                        .px(px(6.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .items_center()
                        .when(open, |this| this.bg(hsla(p.accent)))
                        .when(has_diagnosis, |this| {
                            this.cursor_pointer()
                                .hover(|this| this.bg(hsla(p.accent)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connection_diagnosis_open = !this.connection_diagnosis_open;
                                    cx.notify();
                                }))
                        })
                        .child(crate::connect::status_dot(Some(status.dot()), cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(if error_tone { hsla(p.destructive_foreground) } else { hsla(p.muted_foreground) })
                                .child(status.title()),
                        )
                        .when(!open, |this| {
                            this.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(details.clone()).build(window, cx))
                        }),
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

    /// CN-STATUS: floating connection diagnosis popover (`ConnectionStatusDot.tsx` +
    /// `ConnectionDiagnosisDetails.tsx`), anchored beneath the titlebar status dot.
    fn render_connection_diagnosis_popover(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.connection_diagnosis_open {
            return None;
        }
        let state = self.state.read(cx);
        let Phase::Online(status) = &state.phase else { return None };
        let host = state
            .active_environment()
            .map(|environment| environment.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "this environment".to_owned());
        let diag = diagnose(status, &host)?;
        let copy_text = connection_diagnosis_text(&host, &diag);
        let can_retry = status.can_retry();
        let attempt = status.attempt();
        let subtitle = if attempt > 0 { format!("{host} \u{00b7} Attempt {attempt}") } else { host };
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let panel = v_flex()
            .id("connection-diagnosis-popover")
            .absolute()
            .top(px(TITLEBAR_HEIGHT + 4.))
            .right(px(16.))
            .w(px(360.))
            .max_w(px(400.))
            .rounded(px(12.))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(crate::glass::backdrop(12., hsla(p.popover), hsla(p.border), cx))
            .child(
                v_flex()
                    .relative()
                    .p(px(14.))
                    .gap(px(10.))
                    .text_xs()
                    .child(
                        h_flex()
                            .items_start()
                            .gap(px(8.))
                            .child(div().pt(px(2.)).child(crate::connect::status_dot(Some(status.dot()), cx)))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.destructive_foreground))
                                            .child(diag.headline.clone()),
                                    )
                                    .child(div().text_color(hsla(p.muted_foreground)).child(subtitle)),
                            )
                            .child(
                                crate::right_panel::Button::new("connection-diagnosis-close")
                                    .variant(crate::right_panel::Variant::GhostMuted)
                                    .size(crate::right_panel::Size::Xs)
                                    .icon(Icon::X)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.connection_diagnosis_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(v_flex().gap(px(3.)).children(diag.hops.iter().map(|hop| {
                        let color = match hop.result {
                            HopResult::Failed => hsla(p.destructive_foreground),
                            HopResult::Ok => hsla(p.foreground),
                            HopResult::Unchecked => hsla(p.muted_foreground),
                        };
                        let note = hop.result.note();
                        let line = if note.is_empty() {
                            format!("{} {}", hop.result.mark(), hop.label)
                        } else {
                            format!("{} {} ({note})", hop.result.mark(), hop.label)
                        };
                        div().text_color(color).child(line)
                    })))
                    .when(!diag.steps.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(px(3.))
                                .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("What to try"))
                                .children(
                                    diag.steps
                                        .iter()
                                        .map(|step| div().text_color(hsla(p.muted_foreground)).child(format!("\u{2022} {step}"))),
                                ),
                        )
                    })
                    .when(!diag.facts.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(px(2.))
                                .px(px(8.))
                                .py(px(6.))
                                .rounded(px(6.))
                                .bg(hsla(p.muted).opacity(0.45))
                                .font_family(mono)
                                .text_color(hsla(p.muted_foreground))
                                .children(diag.facts.iter().map(|(k, v)| div().child(format!("{k}: {v}")))),
                        )
                    })
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .when(can_retry, |this| {
                                this.child(
                                    crate::right_panel::Button::new("connection-diagnosis-retry")
                                        .variant(crate::right_panel::Variant::Ghost)
                                        .size(crate::right_panel::Size::Xs)
                                        .label("Retry now")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.connection_diagnosis_open = false;
                                            this.state.read(cx).retry_connection();
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(
                                crate::right_panel::Button::new("connection-diagnosis-copy")
                                    .variant(crate::right_panel::Variant::GhostMuted)
                                    .size(crate::right_panel::Size::Xs)
                                    .label("Copy diagnostics")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                        this.state.update(cx, |state, cx| {
                                            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Info, "Diagnostics copied"), cx);
                                        });
                                    })),
                            )
                            .child(
                                crate::right_panel::Button::new("connection-diagnosis-switch")
                                    .variant(crate::right_panel::Variant::GhostMuted)
                                    .size(crate::right_panel::Size::Xs)
                                    .label("Switch environment")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.connection_diagnosis_open = false;
                                        this.state.update(cx, |state, cx| state.open_switcher(cx));
                                    })),
                            ),
                    ),
            );
        Some(
            gpui::deferred(
                div()
                    .id("connection-diagnosis-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.connection_diagnosis_open = false;
                        cx.notify();
                    }))
                    .child(panel),
            )
            .with_priority(50)
            .into_any_element(),
        )
    }

    fn render_titlebar(&self, left: Pixels, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.render_connection_status(cx);
        let state = self.state.read(cx);
        let has_chat = state.selected_thread.is_some() || state.draft_project.is_some();
        let no_projects = !has_chat
            && state.shell.synchronized
            && state.shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.iter().all(|project| project.archived_at.is_some()));
        let terminal_toggle = has_chat.then(|| terminal::header_toggle(&self.terminal, cx).into_any_element());
        chat_header::container_with_layout(left, !no_projects, cx)
            .child(self.header.clone())
            .children(status)
            .when(!has_chat && !no_projects, |bar| bar.child(new_thread::new_thread_button(cx)))
            .child(
                div()
                    .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                        this.header.update(cx, |header, cx| header.cancel_rename(cx));
                    }))
                    .child(chat_header::controls(&self.state, terminal_toggle, window, cx)),
            )
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
        let tracking_swipe = self.history_swipe.indicator().is_some_and(|indicator| indicator.tracking);
        let swipe_fade = transition(
            "history-swipe-fade",
            f32::from(tracking_swipe),
            if tracking_swipe { Duration::ZERO } else { Duration::from_millis(200) },
            window,
            cx,
        );
        if swipe_fade == 0. {
            self.history_swipe.clear_indicator();
        }
        let swipe_indicator = self.history_swipe.indicator().map(|mut indicator| {
            if !indicator.tracking {
                indicator.opacity = swipe_fade;
                indicator.progress *= swipe_fade;
            }
            indicator
        });
        let pane_height = f32::from(window.viewport_size().height);
        self.sync_theme_background(cx);
        self.sync_window_title(window, cx);
        let state = self.state.read(cx);
        let has_thread = state.selected_thread.is_some();
        let has_draft = !has_thread && state.draft_project.is_some();
        let viewport = f32::from(window.viewport_size().width);
        let sidebar_width = sidebar_inline_width(state, viewport);
        let sidebar_open = sidebar_width > 0.;
        let sidebar_is_sheet = layout::is_sheet(viewport);
        let sheet_open = sidebar_is_sheet && state.sidebar_sheet.is_open();
        let panel_duration = panel_animation_duration(cx);
        let sidebar_mode_changed = self.sidebar_is_sheet.replace(sidebar_is_sheet) != Some(sidebar_is_sheet);
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
        let on_issues = state.issues_open && !on_pull_requests;
        let on_utility_page = on_pull_requests || on_issues;
        let composer_replaced = self.composer_replaced;
        let inline_progress = transition(
            "inline-sidebar",
            f32::from(sidebar_open),
            if sidebar_is_sheet || sidebar_mode_changed { Duration::ZERO } else { panel_duration },
            window,
            cx,
        );
        let sheet_progress = transition(
            "sheet-sidebar",
            f32::from(sheet_open),
            if sidebar_is_sheet && !sidebar_mode_changed { panel_duration } else { Duration::ZERO },
            window,
            cx,
        );
        let titlebar_inset = px(transition(
            "chat-header-inset",
            f32::from(traffic_light_inset(window, !sidebar_open)),
            if sidebar_mode_changed { Duration::ZERO } else { panel_duration },
            window,
            cx,
        ));
        let theme = cx.theme();
        let sidebar_inline_full = if sidebar_open {
            sidebar_width
        } else {
            sidebar_inline_width_when_open(self.state.read(cx), viewport)
        };
        let sidebar_el = (inline_progress > 0.).then(|| {
            panel_width_clip(
                px(sidebar_inline_full * inline_progress),
                px(sidebar_inline_full),
                true,
                self.sidebar_surface(cx),
            )
            .relative()
            .when(!sidebar_open, |clip| clip.child(div().absolute().inset_0().occlude()))
        });
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
                        || crate::voice::handle_setup_escape(cx)
                        || this.snapshot_setup.update(cx, |dialog, cx| dialog.handle_escape(cx)))
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
            .on_action(cx.listener(|this, _: &CopyIssueKey, _, cx| {
                if !this.issues.update(cx, |page, cx| page.copy_selected_key(cx)) {
                    cx.propagate();
                }
            }))
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _, cx| {
                this.sidebar.update(cx, |sidebar, cx| sidebar.set_modifiers(event.modifiers, cx));
                this.pull_requests.update(cx, |page, cx| page.set_modifiers(event.modifiers, cx));
            }))
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if this.connection_diagnosis_open {
                        this.connection_diagnosis_open = false;
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    if this.project_dialogs.update(cx, |dialogs, cx| dialogs.dismiss(cx)) {
                        cx.stop_propagation();
                        return;
                    }
                    let overlay_open = this.menu.read(cx).is_open()
                        || this.dialog.read(cx).is_open()
                        || this.snooze.read(cx).is_open()
                        || this.palette.read(cx).is_open()
                        || this.project_dialogs.read(cx).is_open()
                        || crate::updates::overlay(cx).is_some()
                        || crate::voice::is_setup_modal_open(cx)
                        || this.snapshot_setup.read(cx).is_open()
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
                        || this.project_dialogs.read(cx).is_open()
                        || crate::updates::overlay(cx).is_some()
                        || crate::voice::is_setup_modal_open(cx)
                        || this.snapshot_setup.read(cx).is_open()
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
            .children(sidebar_el)
            .when(on_utility_page, |this| {
                this.child(
                    div().relative().flex_1().h_full().min_w_0()
                        .on_scroll_wheel(cx.listener(Self::handle_history_swipe_wheel))
                        .when(on_pull_requests, |page| page.child(self.pull_requests.clone()))
                        .when(on_issues, |page| page.child(self.issues.clone()))
                        .when_some(swipe_indicator, |page, indicator| {
                            page.child(crate::quit_hold::swipe_indicator(indicator, pane_height, cx))
                        }),
                )
            })
            .when(!maximized && !on_utility_page, |this| {
                this.child(
                v_flex()
                    .relative()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .when_some(
                        (!crate::theme::slot_stylesheet(cx).has_scoped_background_image())
                            .then(|| crate::theme::background_image(cx))
                            .flatten(),
                        |this, image| {
                            this.child(
                                gpui::img(image)
                                    .object_fit(gpui::ObjectFit::Cover)
                                    .size_full()
                                    .absolute()
                                    .inset_0(),
                            )
                        },
                    )
                    .on_scroll_wheel(cx.listener(Self::handle_history_swipe_wheel))
                    .child(self.render_titlebar(titlebar_inset, window, cx))
                    .when(has_thread, |this| {
                        // The inline details card sits over the canvas; the lane moves left to clear it.
                        this.child(
                            v_flex()
                                .relative()
                                .overflow_hidden()
                                .flex_1()
                                .min_h_0()
                                .pr(px(details_layout.lane_inset))
                                .theme_slot_hover(ThemeSlot::ConversationArea, cx)
                                .children(theme_slot_background_layer(ThemeSlot::ConversationArea, cx))
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
                                .overflow_hidden()
                                .flex_1()
                                .min_h_0()
                                .pr(px(details_layout.lane_inset))
                                .theme_slot_hover(ThemeSlot::ConversationArea, cx)
                                .children(theme_slot_background_layer(ThemeSlot::ConversationArea, cx))
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
            .when(!on_utility_page, |this| this.child(div().track_focus(&self.area_focus.right_panel).h_full().child(self.right_panel.clone())))
            .when(sidebar_open && inline_progress == 1., |this| this.child(self.render_sidebar_rail(sidebar_width, cx)))
            .when(sheet_progress > 0., |this| this.child(self.render_sidebar_sheet(layout::sheet_width(viewport), sheet_progress, sheet_open, cx)))
            .when(!sheet_open, |this| this.child(self.render_sidebar_toggle(window, sidebar_open, cx)))
            .children(crate::transcript::html_render::fullscreen(window, cx))
            .children(crate::transcript::mcp_app::fullscreen(window, cx))
            .children(crate::media::fullscreen(window, cx))
            .child(self.prompt_links.clone())
            .child(self.menu.clone())
            .child(self.dialog.clone())
            .child(self.guest_share.clone())
            .child(self.mcp.clone())
            .child(self.git_dialogs.clone())
            .child(self.snooze.clone())
            .child(self.palette.clone())
            .child(self.link_dialog.clone())
            .child(self.project_dialogs.clone())
            .child(self.device_setup.clone())
            .child(self.ssh_prompt.clone())
            .child(self.welcome.clone())
            .children(crate::voice::render_active_voice_setup_modal(cx))
            .children(self.render_connection_diagnosis_popover(cx))
            .children(crate::toast::stack(&self.state, window, cx))
            .children(crate::github_star_prompt::confetti_overlay(&self.state, cx))
            .children(crate::updates::overlay(cx))
            .children(settings)
            .children(theme_editor)
            .children(usage)
            .children(self.snapshot_coordinator.read(cx).render_flash_overlay(cx))
            .child(self.snapshot_setup.clone())
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

    fn sidebar_surface(&self, cx: &Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .track_focus(&self.area_focus.sidebar)
            .relative()
            .size_full()
            .bg(hsla(p.sidebar))
            .border_r_1()
            .border_color(hsla(p.sidebar_edge))
            .theme_slot_hover(ThemeSlot::Sidebar, cx)
            .children(theme_slot_background_layer(ThemeSlot::Sidebar, cx))
            .child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full()))
    }

    fn render_sidebar_sheet(&self, width: f32, progress: f32, open: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .id("sidebar-sheet")
            .absolute()
            .inset_0()
            .when(open, |sheet| sheet.occlude())
            .child(
                div()
                    .id("sidebar-sheet-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(hsla(p.background).opacity(0.6 * progress))
                    .when(open, |backdrop| backdrop.on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |state, cx| state.close_sidebar_sheet(cx));
                    }))),
            )
            .child(
                panel_width_clip(px(width * progress), px(width), true, self.sidebar_surface(cx))
                    .relative()
                    .occlude()
                    .when(!open, |clip| clip.child(div().absolute().inset_0().occlude())),
            )
    }

    fn render_sidebar_toggle(&self, window: &Window, expanded: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let label = crate::i18n::t(cx, "Toggle sidebar");
        let label = keymap::label("sidebar.toggle", cx).map_or_else(|| label.to_owned(), |key| format!("{label} ({key})"));
        div()
            .id("sidebar-toggle")
            .absolute()
            .left(sidebar_toggle_left(window))
            .top(px(12.))
            .size(px(SIDEBAR_TOGGLE_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.sidebar_accent)))
            .child(
                div().relative().size(px(16.))
                    .child(Icon::PanelLeft.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
                    .when(expanded, |icon| icon.child(
                        div().absolute().top(px(4.5)).right(px(1.))
                            .child(Icon::ChevronLeft.sized(px(7.)).text_color(hsla(p.sidebar_icon))),
                    )),
            )
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

    /// `HostFreezeToastCoordinator.ts`: tells the user once per reported stall (`envId:detectedAt`)
    /// why any active or fleet host stopped answering when it reports `lastEventLoopStall`.
    fn handle_host_freeze_toasts(&mut self, cx: &mut Context<Self>) {
        let notices: Vec<(String, String, String)> = {
            let state = self.state.read(cx);
            let mut out = Vec::new();
            let mut seen_keys = HashSet::new();
            let canonical_env_key = |saved_id: &str, saved_env_id: Option<&str>| -> String {
                state
                    .fleet
                    .environments
                    .iter()
                    .find(|view| view.summary.owner.saved_id == saved_id)
                    .and_then(|view| view.config.config.as_ref())
                    .map(|cfg| cfg.environment.environment_id.as_str())
                    .filter(|id| !id.trim().is_empty())
                    .or_else(|| {
                        (state.environments.active.as_deref() == Some(saved_id))
                            .then_some(state.server_config.config.as_ref())
                            .flatten()
                            .map(|cfg| cfg.environment.environment_id.as_str())
                            .filter(|id| !id.trim().is_empty())
                    })
                    .or_else(|| saved_env_id.filter(|id| !id.trim().is_empty()))
                    .unwrap_or(saved_id)
                    .to_owned()
            };
            let mut push_stall = |env_key: String, label: &str, stall: &t3_client::status::ServerEventLoopStall| {
                let dedup = stall.dedup_key(&env_key);
                if !seen_keys.insert(dedup.clone()) {
                    return;
                }
                if let Some(notice) = host_freeze_notice(label, Some(stall)) {
                    out.push((dedup, notice.title, notice.description));
                }
            };
            if let Some(client) = state.client_handle()
                && let Some(stall) = client.server_config_extras().borrow().last_event_loop_stall.clone()
            {
                let env_key = state
                    .active_environment()
                    .map(|env| canonical_env_key(&env.id, env.environment_id.as_deref()))
                    .or_else(|| {
                        state
                            .server_config
                            .config
                            .as_ref()
                            .map(|c| c.environment.environment_id.clone())
                            .filter(|s| !s.trim().is_empty())
                    })
                    .unwrap_or_else(|| "local".to_owned());
                let label = state
                    .active_environment()
                    .map(|e| e.label.clone())
                    .or_else(|| state.server_config.config.as_ref().map(|c| c.environment.label.clone()))
                    .unwrap_or_default();
                push_stall(env_key, &label, &stall);
            }
            for env in &state.environments.environments {
                let extras = t3_client::supervisor::server_config_extras_for(&env.http_base);
                if let Some(stall) = extras.borrow().last_event_loop_stall.as_ref() {
                    let env_key = canonical_env_key(&env.id, env.environment_id.as_deref());
                    let label = if !env.label.trim().is_empty() {
                        env.label.as_str()
                    } else {
                        state
                            .fleet
                            .environments
                            .iter()
                            .find(|view| view.summary.owner.saved_id == env.id)
                            .and_then(|view| view.config.config.as_ref())
                            .map(|cfg| cfg.environment.label.as_str())
                            .unwrap_or_default()
                    };
                    push_stall(env_key, label, stall);
                }
            }
            for env in &state.fleet.environments {
                let Some(saved) = state.environments.get(&env.summary.owner.saved_id) else { continue };
                let extras = t3_client::supervisor::server_config_extras_for(&saved.http_base);
                if let Some(stall) = extras.borrow().last_event_loop_stall.as_ref() {
                    let env_key = canonical_env_key(&saved.id, saved.environment_id.as_deref());
                    let label = if !saved.label.trim().is_empty() {
                        saved.label.as_str()
                    } else {
                        env.config
                            .config
                            .as_ref()
                            .map(|c| c.environment.label.as_str())
                            .unwrap_or_default()
                    };
                    push_stall(env_key, label, stall);
                }
            }
            out
        };
        for (dedup_key, title, description) in notices {
            if self.shown_host_freezes.insert(dedup_key) {
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
        let blocked = self.has_blocking_overlay(window, cx) || self.state.read(cx).sidebar_sheet.is_open();
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
        let return_to_chat = {
            let state = self.state.read(cx);
            (state.issues_open || state.pull_requests_open)
                && (state.selected_thread.is_some() || state.draft_project.is_some())
        };
        let action = {
            let state = self.state.read(cx);
            self.history_swipe.on_wheel(wheel, |dir| {
                (dir.is_back() && return_to_chat) || state.can_navigate(dir.is_back())
            })
        };
        if let SwipeAction::Navigate(dir) = action {
            self.state.update(cx, |state, cx| {
                if dir.is_back() && return_to_chat {
                    state.set_issues_open(false, cx);
                    state.set_pull_requests_open(false, cx);
                } else {
                    state.navigate_history(dir.is_back(), cx);
                }
            });
        }
        let after = self.history_swipe.indicator();
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
        self.connection_diagnosis_open
            || self.menu.read(cx).is_open()
            || self.dialog.read(cx).is_open()
            || self.snooze.read(cx).is_open()
            || self.palette.read(cx).is_open()
            || self.project_dialogs.read(cx).is_open()
            || self.mcp.read(cx).is_open(cx)
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
        let on_issues = state.issues_open && !on_pull_requests;
        let on_utility_page = on_pull_requests || on_issues;
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
        if on_utility_page {
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
        if !on_utility_page && right_panel_open {
            visible.push(WorkspaceFocusArea::RightPanel);
        }

        let current = visible.iter().copied().find(|area| match area {
            WorkspaceFocusArea::Sidebar => self.area_focus.sidebar.contains_focused(window, cx),
            WorkspaceFocusArea::Conversation => self.area_focus.conversation.contains_focused(window, cx),
            WorkspaceFocusArea::Composer => self.area_focus.composer.contains_focused(window, cx),
            WorkspaceFocusArea::Terminal => self.area_focus.terminal.contains_focused(window, cx),
            WorkspaceFocusArea::RightPanel => self.area_focus.right_panel.contains_focused(window, cx),
            WorkspaceFocusArea::PullRequests => {
                if on_issues {
                    self.issues.read(cx).focus_handle(cx).contains_focused(window, cx)
                } else {
                    self.pull_requests.read(cx).focus_handle(cx).contains_focused(window, cx)
                }
            }
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
                let handle = if on_issues {
                    self.issues.read(cx).focus_handle(cx)
                } else {
                    self.pull_requests.read(cx).focus_handle(cx)
                };
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
        if state.pull_requests_open
            || state.issues_open
            || (state.selected_thread.is_none() && state.draft_project.is_none())
        {
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

    /// Syncs the published theme background image for the active theme (`themeBackground.ts`:
    /// `pingdotgg/t3code#36680`, `pingdotgg/t3code#37843`, `pingdotgg/t3code#37981`,
    /// `pingdotgg/t3code#38132`).
    fn sync_theme_background(&mut self, cx: &mut Context<Self>) {
        use t3_core::themes::background::{BackgroundAction, RetryDecision};

        let (environment_id, client, runtime) = {
            let state = self.state.read(cx);
            let env_id = state
                .server_config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .or_else(|| state.environments.active.clone());
            (env_id, state.client_handle(), state.runtime().clone())
        };
        let theme_id = crate::theme::active_theme_id(cx);
        let custom = crate::theme::preferences(cx).custom.clone();
        let environment = crate::theme::environment_themes(cx).to_vec();

        let action = self.theme_background.apply(
            environment_id.as_deref(),
            theme_id.as_deref(),
            &custom,
            &environment,
            client.is_some(),
        );
        match action {
            BackgroundAction::NoChange => {}
            BackgroundAction::Clear => {
                self.theme_background_task = None;
                crate::theme::set_background_image(None, cx);
            }
            BackgroundAction::Fetch { generation, key } => {
                // Clear previous wallpaper immediately so it never shows under the next theme while loading.
                crate::theme::set_background_image(None, cx);
                let Some(client) = client else {
                    self.theme_background_task = None;
                    return;
                };
                let theme_id = key.theme_id;
                let svg = cx.svg_renderer();
                self.theme_background_task = Some(cx.spawn(async move |this, cx| {
                    loop {
                        let fetch_client = client.clone();
                        let fetch_theme_id = theme_id.clone();
                        let fetched = runtime
                            .spawn(async move {
                                t3_client::assets::fetch_theme_background(&fetch_client, &fetch_theme_id).await
                            })
                            .await
                            .ok()
                            .and_then(Result::ok);
                        let decoded = match fetched {
                            Some(bytes) => {
                                let svg = svg.clone();
                                cx.background_executor()
                                    .spawn(async move { decode_theme_background(&bytes, 4096, &svg) })
                                    .await
                            }
                            None => None,
                        };
                        let next_delay_ms = this
                            .update(cx, |this, cx| match decoded {
                                Some(image) => {
                                    if this.theme_background.on_loaded(generation) {
                                        crate::theme::set_background_image(Some(image), cx);
                                    }
                                    None
                                }
                                None => match this.theme_background.on_failed(generation) {
                                    RetryDecision::RetryAfterMs(delay_ms) => Some(delay_ms),
                                    RetryDecision::Stale | RetryDecision::Exhausted => None,
                                },
                            })
                            .ok()
                            .flatten();
                        match next_delay_ms {
                            Some(delay_ms) => {
                                cx.background_executor()
                                    .timer(Duration::from_millis(delay_ms))
                                    .await;
                            }
                            None => break,
                        }
                    }
                }));
            }
        }
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

/// Decodes fetched theme background image bytes into a GPU-ready `RenderImage` off the UI thread.
fn decode_theme_background(
    bytes: &[u8],
    max_edge: u32,
    svg: &gpui::SvgRenderer,
) -> Option<std::sync::Arc<gpui::RenderImage>> {
    if image::guess_format(bytes).is_ok() {
        let img = image::load_from_memory(bytes).ok()?;
        let (w, h) = (img.width(), img.height());
        if w == 0 || h == 0 {
            return None;
        }
        let (target_w, target_h) = t3_core::media::fit_within(w, h, max_edge);
        let img = if (target_w, target_h) == (w, h) { img } else { img.thumbnail_exact(target_w, target_h) };
        let mut pixels = img.into_rgba8();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Some(std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(pixels)])))
    } else {
        let img = svg.render_single_frame(bytes, 2.0).ok()?;
        let size = img.size(0);
        (size.width.0 > 0 && size.height.0 > 0).then_some(img)
    }
}

/// Registers a single process-wide macOS `NSWorkspaceDidWakeNotification` observer so returning
/// from system sleep wakes every connection supervisor (`wakeups.ts`).
#[cfg(all(target_os = "macos", not(test)))]
fn register_macos_wake_observer() {
    use std::sync::OnceLock;
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| unsafe {
        let Some(ws_cls) = AnyClass::get(c"NSWorkspace") else {
            return;
        };
        let workspace: Option<Retained<AnyObject>> = msg_send![ws_cls, sharedWorkspace];
        let Some(workspace) = workspace else {
            return;
        };
        let center: Option<Retained<AnyObject>> = msg_send![&*workspace, notificationCenter];
        let Some(center) = center else {
            return;
        };
        let name = NSString::from_str("NSWorkspaceDidWakeNotification");
        let block = RcBlock::new(|_note: *mut AnyObject| {
            t3_client::wakeups::emit_wakeup(t3_client::wakeups::ConnectionWakeup::ApplicationActiveLong);
        });
        let observer: Option<Retained<AnyObject>> = msg_send![
            &*center,
            addObserverForName: &*name,
            object: std::ptr::null::<AnyObject>(),
            queue: std::ptr::null::<AnyObject>(),
            usingBlock: &*block,
        ];
        // Retain the observer token for the lifetime of the app process.
        std::mem::forget(observer);
    });
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

                // Pull Requests and Issues pages hide composer and are mutually exclusive.
                ws.state.update(cx, |state, _| state.pull_requests_open = true);
                assert!(!ws.composer_visible(window, cx));
                ws.state.update(cx, |state, cx| state.set_issues_open(true, cx));
                assert!(ws.state.read(cx).issues_open);
                assert!(!ws.state.read(cx).pull_requests_open);
                assert!(!ws.composer_visible(window, cx));
                ws.state.update(cx, |state, cx| state.set_issues_open(false, cx));

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
        // Upgrading to modern scopes closes the permission-update toast automatically.
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "role": "client",
                    "permissions": ["orchestration:read", "orchestration:operate", "settings:write"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |_, cx| cx.emit(AppEvent::Phase));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_ne!(
                state.toasts.latest().and_then(|t| t.permission_update_env.as_deref()),
                Some("legacy-env-id"),
                "upgraded session should close the legacy permission-update toast"
            );
        });

        // 6. Host-freeze toast (`CN-STATUS`): fires across saved/fleet environments, canonicalizes
        // `env_key` when `saved.environment_id` is `None` while fleet has `config.environment.environment_id`,
        // and deduplicates by `detectedAt`.
        state.update(cx, |state, _| {
            state.environments.environments.push(t3_client::store::SavedEnvironment {
                id: "fleet-worker".into(),
                environment_id: None,
                label: "Build Box".into(),
                http_base: "http://10.0.0.42:3773".into(),
                access_token: Some("tok".into()),
                local: false,
                disabled: false,
                ssh: None,
                server_version: None,
            });
            let mut cfg_state = t3_core::server_config::ServerConfigState::default();
            let snapshot: t3_core::server_config::ServerConfigStreamEvent = serde_json::from_value(json!({
                "version": 1,
                "type": "snapshot",
                "config": {
                    "environment": { "environmentId": "fleet-env-1", "label": "Build Box", "serverVersion": "0.1.9" }
                }
            }))
            .unwrap();
            cfg_state.apply(snapshot);
            let fleet = std::sync::Arc::make_mut(&mut state.fleet);
            let owner = t3_core::sidebar::grouping::EnvironmentOwner {
                saved_id: "fleet-worker".into(),
                canonical_id: Some("fleet-env-1".into()),
                generation: 1,
                connection_generation: 1,
                shell_generation: 0,
            };
            fleet.environments.push(std::sync::Arc::new(t3_client::environments::EnvironmentView {
                summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                    owner,
                    &t3_core::shell::ShellState::default(),
                    &cfg_state,
                )),
                status: ConnectionStatus::Connected,
                config: cfg_state,
                grants: None,
                settings_revision: 0,
                keybindings_revision: 0,
            }));
        });
        t3_client::supervisor::set_server_config_extras_for(
            "http://10.0.0.42:3773",
            t3_client::supervisor::ServerConfigExtras {
                last_event_loop_stall: Some(t3_client::status::ServerEventLoopStall {
                    detected_at: Some("2026-11-19T01:00:00.000Z".into()),
                    duration_ms: 8400.0,
                    load_average: Some(12.0),
                    cpu_count: 4,
                }),
                ..Default::default()
            },
        );
        state.update(cx, |_, cx| cx.emit(AppEvent::ServerConfig));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let latest = state.toasts.latest().expect("host freeze toast");
            assert_eq!(latest.kind, crate::toast::ToastKind::Warning);
            assert_eq!(latest.title, "Build Box froze for 8s");
            assert!(latest.description.as_deref().unwrap_or_default().contains("load 12 on 4 cores"));
        });
        let shown_after_first = workspace.read_with(cx, |ws, _| ws.shown_host_freezes.len());
        assert_eq!(shown_after_first, 1);
        // Re-emitting with the same `detectedAt` must not duplicate the toast.
        state.update(cx, |state, cx| {
            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Info, "sentinel"), cx);
            cx.emit(AppEvent::ServerConfig);
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.toasts.latest().map(|t| t.title.as_str()),
                Some("sentinel"),
                "same detectedAt must not re-fire host-freeze toast"
            );
        });
        assert_eq!(workspace.read_with(cx, |ws, _| ws.shown_host_freezes.len()), 1);
        // A newer `detectedAt` fires a fresh toast.
        t3_client::supervisor::set_server_config_extras_for(
            "http://10.0.0.42:3773",
            t3_client::supervisor::ServerConfigExtras {
                last_event_loop_stall: Some(t3_client::status::ServerEventLoopStall {
                    detected_at: Some("2026-11-19T01:05:00.000Z".into()),
                    duration_ms: 6100.0,
                    load_average: None,
                    cpu_count: 4,
                }),
                ..Default::default()
            },
        );
        state.update(cx, |_, cx| cx.emit(AppEvent::ServerConfig));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.toasts.latest().map(|t| t.title.as_str()), Some("Build Box froze for 6s"));
        });
        assert_eq!(workspace.read_with(cx, |ws, _| ws.shown_host_freezes.len()), 2);
        t3_client::supervisor::set_server_config_extras_for("http://10.0.0.42:3773", Default::default());

        // 7. Connection status dot diagnosis popover (`CN-STATUS`): opens while reconnecting/errored
        // and auto-closes once the phase returns to `Connected`.
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Reconnecting {
                attempt: 2,
                reason: "dial tcp 127.0.0.1:3773: connection refused".into(),
            });
            cx.emit(AppEvent::Phase);
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                assert!(ws.render_connection_status(cx).is_some());
                assert!(ws.render_connection_diagnosis_popover(cx).is_none());
                ws.connection_diagnosis_open = true;
                assert!(ws.has_blocking_overlay(window, cx));
                assert!(ws.render_connection_diagnosis_popover(cx).is_some());
            });
        })
        .unwrap();
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            cx.emit(AppEvent::Phase);
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, _, cx| {
            workspace.read_with(cx, |ws, _| {
                assert!(!ws.connection_diagnosis_open, "popover must auto-close on Connected");
            });
        })
        .unwrap();

        // 8. SlowRpcRequestToastCoordinator (`AP-TOASTS`): surfaces warning toast when slow RPCs
        // arrive and dismisses it automatically when all slow RPCs complete.
        t3_client::session::publish_slow_rpc_requests_for_test(vec![t3_client::session::SlowRpcAckRequest {
            request_id: "ws-slow-1".into(),
            started_at_ms: 1_700_000_000_000,
            tag: "orchestration.dispatchCommand · Local".into(),
            threshold_ms: 15_000,
        }]);
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let latest = state.toasts.latest().expect("slow rpc warning toast");
            assert_eq!(latest.kind, crate::toast::ToastKind::Warning);
            assert_eq!(latest.title, "Some requests are slow");
            assert_eq!(latest.description.as_deref(), Some("1 request waiting longer than 15s."));
        });
        t3_client::session::publish_slow_rpc_requests_for_test(Vec::new());
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_ne!(
                state.toasts.latest().map(|t| t.title.as_ref()),
                Some("Some requests are slow"),
                "slow rpc toast must close once pending slow requests clear"
            );
        });

        // 9. `panelAnimationDurationMs` (`#1`) drives left sidebar open/close transitions, `Shift+F6`
        // moves focus from right-panel/side-chat back to main chat (`pingdotgg/t3code#31931`), and
        // right-panel shortcuts work on unsent draft threads (`pingdotgg/t3code#35189`).
        cx.update(|cx| {
            crate::settings::store::install_committed_client(
                &state,
                serde_json::from_value(json!({ "panelAnimationDurationMs": 180 })).unwrap(),
                cx,
            );
        });
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                let anim_ms = crate::settings::store::try_client(cx)
                    .map_or(0, |doc| doc.panel_animation_duration_ms());
                assert_eq!(anim_ms, 180);
                // `Shift+F6` from inside right-panel (including side chat) moves focus to main chat.
                ws.area_focus.right_panel.focus(window, cx);
                assert!(ws.area_focus.right_panel.contains_focused(window, cx));
                ws.move_workspace_focus(false, window, cx);
                assert!(!ws.area_focus.right_panel.contains_focused(window, cx));

                // Unsent draft thread still has a `panel_thread()` and opens the right panel cleanly.
                ws.state.update(cx, |state, cx| {
                    state.start_draft(&expected_project, cx);
                    assert_eq!(state.panel_thread().as_deref(), Some(format!("draft:{expected_project}").as_str()));
                    state.toggle_right_panel(cx);
                    let panel_key = state.panel_thread().expect("draft panel thread");
                    assert!(state.panels.panels.panel(&panel_key).is_open);
                });
            });
        })
        .unwrap();

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn theme_css_hooks_chrome_panels_render_cleanly_with_preview_and_scoped_background_image(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::composer::init(cx);
            crate::keymap::init(cx);
            let installed =
                crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
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
                    "permissions": ["orchestration:read", "orchestration:operate", "terminal:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            let snapshot = state.shell.snapshot.as_mut().expect("fixture snapshot");
            let project_id = snapshot.projects[0].id.clone();
            snapshot.projects[0].project_icon =
                Some(json!({ "kind": "monogram", "text": "T3", "color": "blue" }));
            // Add a running/working thread and a settled thread alongside `menu-thread` so
            // active, inactive, and receding-working `row_surface` branches all render.
            snapshot.threads.push(
                serde_json::from_value(json!({
                    "id": "working-thread",
                    "projectId": project_id,
                    "title": "Working row",
                    "createdAt": "2026-06-01T00:00:00Z",
                    "updatedAt": "2026-06-01T00:00:00Z",
                    "providerInstanceId": "replay",
                    "modelSelection": { "instanceId": "replay", "model": "replay" },
                    "runtimeMode": "auto",
                    "interactionMode": "default",
                    "lineage": { "rootThreadId": "working-thread" },
                    "status": "running",
                    "latestRunId": "run-1",
                    "activeRunId": "run-1",
                    "activityRunStartedAt": "2026-06-01T00:00:00Z",
                    "latestUserAuthoredMessageAt": "2026-06-01T00:00:00Z",
                    "hasActionableProposedPlan": false,
                    "itemCount": 0,
                    "visibleItemCount": 0
                }))
                .unwrap(),
            );
            snapshot.threads.push(
                serde_json::from_value(json!({
                    "id": "inactive-thread",
                    "projectId": project_id,
                    "title": "Inactive row",
                    "createdAt": "2026-06-01T00:00:00Z",
                    "updatedAt": "2026-06-01T00:00:00Z",
                    "providerInstanceId": "replay",
                    "modelSelection": { "instanceId": "replay", "model": "replay" },
                    "runtimeMode": "auto",
                    "interactionMode": "default",
                    "lineage": { "rootThreadId": "inactive-thread" },
                    "status": "idle",
                    "hasActionableProposedPlan": false,
                    "itemCount": 0,
                    "visibleItemCount": 0
                }))
                .unwrap(),
            );
            state.rebuild_sidebar(cx);
            state.select_thread("menu-thread", cx);
            state.sidebar.collapsed = false;
        });

        let (workspace, vcx) = cx.add_window_view(|window, cx| {
            let mut ws = Workspace::new(state.clone(), window, cx);
            // Suppress Composer's native macOS `Input` during `TestWindow::draw` (`gpui-component`
            // calls unimplemented `TestWindow::window_handle` in `set_text_content_type`).
            ws.composer_replaced = true;
            ws
        });
        vcx.run_until_parked();

        // Install a synthetic 2x2 RenderImage so `theme_slot_background_layer` produces live elements.
        let synthetic_bg = std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([0x22, 0x44, 0x88, 0xff])),
        )]));

        let all_nine_css = r#"
            [data-slot="sidebar"] {
                background: var(--theme-background-image) center / cover no-repeat;
                border-right: 2px solid #445566;
            }
            [data-slot="sidebar"]:hover {
                border-color: #778899;
            }
            [data-slot="sidebar-inner"] {
                padding: 2px;
                border-color: #334455;
            }
            [data-slot="sidebar-inner"]:hover {
                opacity: 0.98;
            }
            [data-slot="sidebar-inner"]:active {
                opacity: 0.96;
            }
            [data-thread-item] {
                border-radius: 10px;
                border: 1px solid #223344;
            }
            [data-thread-item]:hover {
                background: #223344;
                opacity: 1;
            }
            [data-thread-item]:active {
                background: #112233;
            }
            [data-slot="project-monogram"] {
                background: #102030;
                color: #ffaa00;
                border: 1px solid #ffaa00;
                border-radius: 999px;
            }
            [data-slot="project-monogram"]:hover {
                opacity: 0.9;
            }
            [data-chat-header] {
                background: #121826;
                border-bottom: 2px solid #335577;
            }
            [data-chat-header]:hover {
                opacity: 0.99;
            }
            [data-workspace-area="conversation"] {
                background: var(--theme-background-image) center / cover no-repeat;
            }
            [data-thread-details-card] {
                background: #161b26;
                border: 2px solid #446688;
                border-radius: 12px;
            }
            [data-thread-details-card]:hover {
                border-color: #6688aa;
            }
            [data-thread-details-card]:active {
                opacity: 0.98;
            }
            [data-thread-terminal-drawer] {
                background: #0b0e14;
                border-top: 2px solid #336699;
            }
            [data-thread-terminal-drawer]:hover {
                border-color: #4477aa;
            }
            [data-thread-terminal-drawer]:active {
                opacity: 0.99;
            }
            [data-settings-page-scroll] {
                background: #111520;
                padding: 20px;
            }
            [data-settings-page-scroll]:hover {
                opacity: 0.99;
            }
            [data-settings-page-scroll]:active {
                opacity: 0.98;
            }
        "#;

        vcx.update(|window, cx| {
            crate::theme::set_background_image(Some(synthetic_bg.clone()), cx);
            let colors = t3_core::themes::standard(true);
            crate::theme::preview_with_css(Some((&colors, Some(all_nine_css), true)), cx);

            assert!(crate::theme::slot_stylesheet(cx).has_scoped_background_image());
            assert!(theme_slot_background_layer(ThemeSlot::Sidebar, cx).is_some());
            assert!(theme_slot_background_layer(ThemeSlot::ConversationArea, cx).is_some());
            assert!(theme_slot_background_layer(ThemeSlot::ChatHeader, cx).is_none());

            // Open terminal drawer and suppress Composer's native macOS `Input` during `TestWindow::draw`
            // (`gpui-component` calls unimplemented `TestWindow::window_handle` in `set_text_content_type`).
            workspace.update(cx, |ws, cx| {
                ws.composer_replaced = true;
                ws.terminal.update(cx, |drawer, cx| drawer.toggle(window, cx));
            });

            // 1. Active thread + inline sidebar + chat header + conversation area + inline details card + terminal drawer.
            window.blur();
            window.refresh();
            window.draw(cx).clear();

            // 2. Narrow sheet sidebar (`render_sidebar_sheet`) + popover details card.
            state.update(cx, |state, cx| {
                state.set_sidebar_viewport(500., cx);
                state.toggle_sidebar_or_sheet(cx);
            });
            if let Some(details) = crate::thread_details::details(cx) {
                details.update(cx, |details, cx| {
                    details.set_presentation(crate::thread_details::Presentation::Popover);
                    details.toggle("menu-thread", cx);
                });
            }
            window.blur();
            window.refresh();
            window.draw(cx).clear();

            // 3. Settings modal (scrollable section + list-backed section) + scoped wallpaper on remaining slots.
            let scoped_layers_css = r#"
                [data-slot="sidebar-inner"] { background: var(--theme-background-image) center / cover no-repeat; }
                [data-chat-header] { background: var(--theme-background-image) center / cover no-repeat; }
                [data-thread-details-card] { background: var(--theme-background-image) center / cover no-repeat; }
                [data-thread-terminal-drawer] { background: var(--theme-background-image) center / cover no-repeat; }
                [data-settings-page-scroll] { background: var(--theme-background-image) center / cover no-repeat; }
                [data-thread-item]:hover { background: #334455; }
            "#;
            crate::theme::preview_with_css(Some((&colors, Some(scoped_layers_css), true)), cx);
            for slot in [
                ThemeSlot::SidebarInner,
                ThemeSlot::ChatHeader,
                ThemeSlot::ThreadDetailsCard,
                ThemeSlot::ThreadTerminalDrawer,
                ThemeSlot::SettingsPageScroll,
            ] {
                assert!(
                    theme_slot_background_layer(slot, cx).is_some(),
                    "expected scoped background image layer for {slot:?}"
                );
            }
            state.update(cx, |state, cx| {
                state.set_sidebar_viewport(1200., cx);
                state.select_thread("menu-thread", cx);
            });
            crate::settings::open(t3_core::settings::search::Section::Appearance, cx);
            window.blur();
            window.refresh();
            window.draw(cx).clear();

            crate::settings::open(t3_core::settings::search::Section::Projects, cx);
            window.blur();
            window.refresh();
            window.draw(cx).clear();

            // Clear live preview and background image; stock theme restores cleanly with zero overrides.
            crate::theme::preview(None, cx);
            crate::theme::set_background_image(None, cx);
            assert!(crate::theme::slot_stylesheet(cx).is_empty());
            assert!(theme_slot_background_layer(ThemeSlot::ConversationArea, cx).is_none());
            window.blur();
            window.refresh();
            window.draw(cx).clear();
        });

        state.update(vcx, |state, _| state.stop_pump());
        vcx.cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        vcx.run_until_parked();
    }
}

#[cfg(test)]
mod panel_clip_tests {
    use super::*;
    use crate::components::motion::AnimatedHeight;

    struct Panels {
        width: Pixels,
        drawer_open: bool,
    }

    impl Render for Panels {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex().size_full()
                .child(div().h(px(180.)).child(panel_width_clip(
                    self.width, px(240.), true,
                    div().id("test-width-inner").size_full().debug_selector(|| "test-width-inner".into()),
                )))
                .child(AnimatedHeight::new(
                    "test-drawer", px(180.), self.drawer_open, Duration::from_millis(400),
                    div().id("test-height-inner").size_full().debug_selector(|| "test-height-inner".into()),
                ))
        }
    }

    #[gpui::test]
    fn panel_chrome_closing_clip_preserves_mounted_panel_bounds(cx: &mut gpui::TestAppContext) {
        let (panels, vcx) = cx.add_window_view(|_, _| Panels { width: px(240.), drawer_open: true });
        vcx.refresh().unwrap();
        let width = vcx.debug_bounds("test-width-inner").unwrap().size;
        let height = vcx.debug_bounds("test-height-inner").unwrap().size;
        assert_eq!(width.width, px(240.));
        assert_eq!(height.height, px(180.));

        panels.update(vcx, |panels, cx| {
            panels.width = px(64.);
            panels.drawer_open = false;
            cx.notify();
        });
        vcx.refresh().unwrap();
        assert_eq!(vcx.debug_bounds("test-width-inner").unwrap().size, width);
        assert_eq!(vcx.debug_bounds("test-height-inner").unwrap().size, height);
    }
}
