//! Thread details (ledger TV-DETAILS): the 280px card of workspace, version control and lineage
//! controls beside the transcript. Ports `components/chat/ThreadDetailsPanel.tsx`,
//! `ThreadDetailsCard.tsx`, `threadDetailsCardLayout.ts`, the card's per-thread visibility in
//! `rightPanelStore.ts`, and `threadPanel.toggle`. Inline at the canvas's top-right when the
//! transcript lane still fits beside it, else a popover under the header's toggle.

mod automations;
pub(crate) mod controls;
mod items;
mod sections;
mod servers;
mod usage;
mod view;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Action, App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task, Window, actions};
use serde::{Deserialize, Serialize};
use t3_client::store;
use t3_core::project_scripts::{ProjectScript, preferred_script, resolve_project_scripts};
use t3_core::right_panel::{DEFAULT_WIDTH, clamp_width};
use t3_core::server_config::ServerSettings;
use t3_core::vcs;

pub(crate) use view::DetailsViewPeers;
pub use view::DetailsView;

use crate::app_state::AppState;
use crate::git::GitStore;
use crate::theme::metrics;
use crate::workspace::sidebar_inline_width;

actions!(thread_details, [ToggleThreadPanel, OpenFavoriteEditor]);

/// `script.<id>.run`.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = thread_details, no_json)]
pub struct RunProjectScript {
    pub script_id: String,
}

const STORE: &str = "thread_details";
/// `THREAD_DETAILS_CARD_GAP`.
pub const CARD_GAP: f32 = 12.;
/// `DETAILS_CARD_CLEARANCE` and the lane's `padding` and `minChatWidth` (`chatCanvasLayout.ts`).
const CLEARANCE: f32 = 32.;
const LANE_PADDING: f32 = 48.;
const MIN_CHAT_WIDTH: f32 = 640.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    Inline,
    Popover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PanelVisibility {
    inline_open: bool,
    popover_open: bool,
}

impl Default for PanelVisibility {
    /// `DEFAULT_THREAD_PANEL_VISIBILITY`: inline open, popover closed.
    fn default() -> Self {
        PanelVisibility { inline_open: true, popover_open: false }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    panels: HashMap<String, PanelVisibility>,
    items: HashMap<String, bool>,
    legacy_usage_cleared: bool,
    last_scripts: HashMap<String, String>,
    last_editor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsEvent {
    Changed,
}

/// The card's preferences and the open thread's Git store.
pub struct Details {
    state: Entity<AppState>,
    pub git: Entity<GitStore>,
    panels: HashMap<String, PanelVisibility>,
    pub items: items::Visibility,
    legacy_usage_enabled: Option<bool>,
    legacy_usage_cleared: bool,
    last_scripts: HashMap<String, String>,
    last_editor: Option<String>,
    /// The mode the card last rendered in, so the toggle acts on what is on screen.
    presentation: Presentation,
    /// A press outside closes the popover just before the toggle's click would reopen it.
    popover_closed_at: Option<Instant>,
    remote_capable_editors: Option<Vec<String>>,
    remote_editor_probe: Option<Task<()>>,
    _git_subscription: Option<Subscription>,
}

impl EventEmitter<DetailsEvent> for Details {}

struct DetailsGlobal(Entity<Details>);

impl Global for DetailsGlobal {}

pub fn details(cx: &App) -> Option<Entity<Details>> {
    cx.try_global::<DetailsGlobal>().map(|global| global.0.clone())
}

fn legacy_usage_flag(cleared: bool, raw_client_settings: Option<&str>) -> Option<bool> {
    if cleared {
        return None;
    }
    raw_client_settings
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("threadDetailsUsageEnabled").and_then(serde_json::Value::as_bool))
}

fn clear_open_popovers(panels: &mut HashMap<String, PanelVisibility>) {
    for visibility in panels.values_mut() {
        visibility.popover_open = false;
    }
}

impl Details {
    fn new(state: Entity<AppState>, git: Entity<GitStore>, cx: &App) -> Self {
        let persisted: Persisted = store::load_ui_state(STORE).and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
        let (items, legacy_usage_enabled, legacy_usage_cleared) = if let Some(doc) = crate::settings::store::try_client(cx) {
            if doc.entries().contains_key("threadDetailsItems")
                || doc.entries().contains_key("threadDetailsUsageEnabled")
                || persisted.items.is_empty()
            {
                (
                    doc.thread_details_visibility(),
                    Some(doc.thread_details_usage_enabled()),
                    !doc.thread_details_usage_enabled(),
                )
            } else {
                (
                    items::Visibility::from_overrides(persisted.items),
                    legacy_usage_flag(persisted.legacy_usage_cleared, store::load_client_settings().as_deref()),
                    persisted.legacy_usage_cleared,
                )
            }
        } else {
            (
                items::Visibility::from_overrides(persisted.items),
                legacy_usage_flag(persisted.legacy_usage_cleared, store::load_client_settings().as_deref()),
                persisted.legacy_usage_cleared,
            )
        };
        Details {
            state,
            git,
            panels: persisted.panels,
            items,
            legacy_usage_enabled,
            legacy_usage_cleared,
            last_scripts: persisted.last_scripts,
            last_editor: persisted.last_editor,
            presentation: Presentation::Inline,
            popover_closed_at: None,
            remote_capable_editors: None,
            remote_editor_probe: None,
            _git_subscription: None,
        }
    }

    /// Mirrors `threadDetailsItems` / `threadDetailsUsageEnabled` from `ClientSettingsDoc`
    /// (`AppearanceSettingsPanel.tsx` -> `ThreadDetailsItemsRow`).
    pub fn sync_from_client_settings(
        &mut self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        emit: bool,
        cx: &mut Context<Self>,
    ) {
        let next_items = doc.thread_details_visibility();
        let next_legacy = Some(doc.thread_details_usage_enabled());
        let next_cleared = !doc.thread_details_usage_enabled();
        if self.items != next_items
            || self.legacy_usage_enabled != next_legacy
            || self.legacy_usage_cleared != next_cleared
        {
            self.items = next_items;
            self.legacy_usage_enabled = next_legacy;
            self.legacy_usage_cleared = next_cleared;
            if emit {
                self.save(cx);
            }
        }
    }

    fn observe_git(&mut self, cx: &mut Context<Self>) {
        if self._git_subscription.is_some() {
            return;
        }
        let git = self.git.clone();
        self._git_subscription = Some(cx.subscribe(&git, |_, _, event: &crate::git::GitEvent, cx| {
            if *event == crate::git::GitEvent::Changed {
                cx.emit(DetailsEvent::Changed);
            }
        }));
    }

    pub(crate) fn start_remote_editor_probe(&mut self, cx: &mut Context<Self>) {
        if self.remote_editor_probe.is_some() || self.remote_capable_editors.is_some() {
            return;
        }
        let background = cx.background_executor().clone();
        self.remote_editor_probe = Some(cx.spawn(async move |this, cx| {
            let probed = background.spawn(async { t3_core::thread_details::probe_remote_capable_editors() }).await;
            let _ = this.update(cx, |details, cx| {
                details.remote_editor_probe = None;
                if !probed.is_empty() && details.remote_capable_editors.is_none() {
                    details.remote_capable_editors = Some(probed);
                    cx.emit(DetailsEvent::Changed);
                    cx.notify();
                }
            });
        }));
    }

    pub fn remote_capable_editors(&self) -> Vec<String> {
        self.remote_capable_editors
            .clone()
            .unwrap_or_else(|| t3_core::thread_details::REMOTE_FALLBACK_EDITORS.iter().map(|id| (*id).to_owned()).collect())
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn set_remote_capable_editors(&mut self, editors: Option<Vec<String>>) {
        self.remote_capable_editors = editors;
    }

    pub fn shows(&self, id: &str) -> bool {
        self.items.shows_with_legacy_usage(id, self.legacy_usage_enabled)
    }

    pub fn items_are_default(&self) -> bool {
        self.items.is_default_with_legacy_usage(self.legacy_usage_enabled)
    }

    fn save(&self, cx: &mut Context<Self>) {
        let persisted = Persisted {
            panels: self.panels.iter().filter(|(_, visibility)| **visibility != PanelVisibility::default()).map(|(id, v)| (id.clone(), *v)).collect(),
            items: self.items.overrides().clone(),
            legacy_usage_cleared: self.legacy_usage_cleared,
            last_scripts: self.last_scripts.clone(),
            last_editor: self.last_editor.clone(),
        };
        store::save_ui_state(STORE, &persisted);
        cx.emit(DetailsEvent::Changed);
    }

    pub fn is_open(&self, thread_id: &str, presentation: Presentation) -> bool {
        let visibility = self.panels.get(thread_id).copied().unwrap_or_default();
        match presentation {
            Presentation::Inline => visibility.inline_open,
            Presentation::Popover => visibility.popover_open,
        }
    }

    pub(crate) fn set_presentation(&mut self, presentation: Presentation) {
        if presentation == Presentation::Inline && self.presentation != Presentation::Inline {
            clear_open_popovers(&mut self.panels);
        }
        self.presentation = presentation;
    }

    /// `toggleThreadPanel` in the mode the card is shown in.
    pub fn toggle(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let recently_closed = self.popover_closed_at.take().is_some_and(|at| at.elapsed() < Duration::from_millis(250));
        let visibility = self.panels.entry(thread_id.to_owned()).or_default();
        match self.presentation {
            Presentation::Inline => visibility.inline_open = !visibility.inline_open,
            Presentation::Popover if recently_closed => {}
            Presentation::Popover => visibility.popover_open = !visibility.popover_open,
        }
        self.save(cx);
    }

    pub fn close_popover(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if let Some(visibility) = self.panels.get_mut(thread_id).filter(|v| v.popover_open) {
            visibility.popover_open = false;
            self.popover_closed_at = Some(Instant::now());
            self.save(cx);
        }
    }

    pub fn set_item(&mut self, id: &str, visible: bool, cx: &mut Context<Self>) {
        self.items.set_with_legacy_usage(id, visible, self.legacy_usage_enabled);
        self.save(cx);
        if crate::settings::store::try_client(cx).is_some() {
            crate::settings::store::update_client(
                &self.state,
                t3_core::client_settings::ClientSettingChange::ThreadDetailsItem {
                    id: id.to_owned(),
                    visible,
                },
                cx,
            );
        }
    }

    pub fn reset_items(&mut self, cx: &mut Context<Self>) {
        self.legacy_usage_enabled = Some(false);
        self.legacy_usage_cleared = true;
        self.items.reset();
        self.save(cx);
        if crate::settings::store::try_client(cx).is_some() {
            crate::settings::store::update_client(
                &self.state,
                t3_core::client_settings::ClientSettingChange::ResetThreadDetailsItems,
                cx,
            );
        }
    }

    pub fn last_script(&self, project_id: &str) -> Option<&str> {
        self.last_scripts.get(project_id).map(String::as_str)
    }

    pub fn preferred_editor(&self, available: &[String]) -> Option<&'static str> {
        vcs::preferred_editor(self.last_editor.as_deref(), available)
    }

    pub fn set_last_editor(&mut self, editor: &str, cx: &mut Context<Self>) {
        if self.last_editor.as_deref() != Some(editor) {
            self.last_editor = Some(editor.to_owned());
            self.save(cx);
        }
    }

    /// `runProjectScript`, remembering the script as the project's Run button.
    pub fn run_script(&mut self, project_id: &str, script: &ProjectScript, cx: &mut Context<Self>) {
        if !self.git.read(cx).can_run_scripts() {
            return;
        }
        if self.last_scripts.get(project_id) != Some(&script.id) {
            self.last_scripts.insert(project_id.to_owned(), script.id.clone());
            self.save(cx);
        }
        self.git.update(cx, |git, cx| git.run_script(script, cx));
    }

    pub fn open_in_editor(&mut self, state: &Entity<AppState>, editor: &str, cx: &mut Context<Self>) {
        use t3_core::thread_details::{RemoteOpenState, build_remote_open_url};

        let remote_state = state.read(cx).active_remote_open_state();
        match remote_state {
            RemoteOpenState::RemoteUnavailable => {}
            RemoteOpenState::RemoteLinks(host) => {
                let cwd = self.git.read(cx).target.as_ref().map(|target| target.cwd.clone()).or_else(|| {
                    let app = state.read(cx);
                    match app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) {
                        Some(thread) => thread.worktree_path.clone().or_else(|| app.shell.project(&thread.project_id).map(|p| p.workspace_root.clone())),
                        None => app.draft_project.as_deref().and_then(|id| app.shell.project(id).map(|p| p.workspace_root.clone())),
                    }
                });
                let Some(cwd) = cwd else { return };
                let Some(url) = build_remote_open_url(editor, &host, &cwd, false) else { return };
                self.set_last_editor(editor, cx);
                cx.open_url(&url);
                let label = vcs::editor_label(editor, cfg!(target_os = "macos"));
                state.update(cx, |state, cx| state.announce(format!("Opened in {label}"), None, cx));
            }
            RemoteOpenState::LocalExec => {
                if !self.git.read(cx).can_operate_thread() {
                    return;
                }
                self.set_last_editor(editor, cx);
                self.git.update(cx, |git, cx| git.open_in_editor(editor, cx));
            }
        }
    }
}

pub(crate) fn panel_key_for(selected_thread: Option<&str>, draft_project: Option<&str>) -> Option<String> {
    selected_thread.map(str::to_owned).or_else(|| draft_project.map(|id| format!("draft:{id}")))
}

/// The active card target key: the selected thread id, or `draft:<project_id>` for a project draft.
pub fn panel_key(state: &AppState) -> Option<String> {
    panel_key_for(state.selected_thread.as_deref(), state.draft_project.as_deref())
}

/// The open thread or draft's effective project scripts: the shell's, as server settings override
/// them (`resolveProjectScripts` in `ChatView.tsx`).
pub fn project_scripts(state: &AppState) -> Option<(String, Vec<ProjectScript>)> {
    let project_id = state
        .selected_thread
        .as_deref()
        .and_then(|id| state.thread_shell(id))
        .map(|thread| thread.project_id.as_str())
        .or(state.draft_project.as_deref())?;
    let project = state.shell.project(project_id)?;
    let own = project.scripts.iter().filter_map(|value| serde_json::from_value(value.clone()).ok()).collect();
    let scripts = match state.server_config.config.as_ref() {
        Some(config) => resolve_project_scripts(&config.settings, &project.id, own),
        None => resolve_project_scripts(&ServerSettings::default(), &project.id, own),
    };
    Some((project.id.clone(), scripts))
}

/// How wide the chat canvas is: the window minus the sidebar and an inline right panel.
pub(crate) fn canvas_width(state: &AppState, viewport_width: f32) -> f32 {
    let sidebar = sidebar_inline_width(state, viewport_width);
    let row = viewport_width - sidebar;
    let panel = state.panel_thread().as_deref().map_or(0., |thread| {
        let panel = state.panels.panels.panel(thread);
        if !panel.is_open || panel.maximized {
            return 0.;
        }
        clamp_width(state.panels.panels.width(thread).unwrap_or(DEFAULT_WIDTH), viewport_width, row)
    });
    row - panel
}

/// `resolveThreadDetailsCardLayout`'s fit test: inline while the lane keeps its minimum width.
pub fn presentation_for(canvas_width: f32) -> Presentation {
    let x = canvas_width - metrics::THREAD_DETAILS_PANEL_WIDTH - CARD_GAP;
    if x - CLEARANCE - LANE_PADDING < MIN_CHAT_WIDTH { Presentation::Popover } else { Presentation::Inline }
}

/// The right inset that keeps the centered lane clear of the inline card
/// (`resolveChatCanvasLayout`'s `laneRight`): none once the lane fits beside it.
pub fn lane_inset_for(canvas_width: f32) -> f32 {
    let card_reach = metrics::THREAD_DETAILS_PANEL_WIDTH + CARD_GAP + CLEARANCE;
    (metrics::CHAT_CONTENT_MAX_WIDTH + 2. * card_reach - canvas_width).clamp(0., card_reach - LANE_PADDING)
}

/// What the workspace needs to place the transcript around the card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub presentation: Presentation,
    pub card_open: bool,
    pub lane_inset: f32,
}

pub fn layout(state: &Entity<AppState>, window: &Window, cx: &App) -> Layout {
    let app = state.read(cx);
    let viewport = window.viewport_size();
    let width = canvas_width(app, f32::from(viewport.width));
    let presentation = presentation_for(width);
    let card_open = match (panel_key(app).as_deref(), details(cx)) {
        (Some(key), Some(details)) => details.read(cx).is_open(key, presentation),
        _ => false,
    };
    let lane_inset = if card_open && presentation == Presentation::Inline { lane_inset_for(width) } else { 0. };
    Layout { presentation, card_open, lane_inset }
}

pub(crate) fn available_height_for(viewport_height: f32, terminal_height: f32, top_inset: f32) -> f32 {
    (viewport_height - crate::workspace::TITLEBAR_HEIGHT - terminal_height.max(0.) - top_inset.max(0.) - 2. * CARD_GAP - 24.).max(0.)
}

pub fn available_height(window: &Window, terminal_height: f32, top_inset: f32) -> f32 {
    available_height_for(f32::from(window.viewport_size().height), terminal_height, top_inset)
}

pub(crate) fn point_in_find_bar(find_open: bool, position: gpui::Point<gpui::Pixels>, state: &AppState, window: &Window) -> bool {
    if !find_open {
        return false;
    }
    let viewport_width = f32::from(window.viewport_size().width);
    let canvas_right = sidebar_inline_width(state, viewport_width) + canvas_width(state, viewport_width);
    let right = canvas_right - 12.;
    let left = right - 280.;
    let top = crate::workspace::TITLEBAR_HEIGHT + 12.;
    let bottom = top + 36.;
    let (x, y) = (f32::from(position.x), f32::from(position.y));
    x >= left && x <= right && y >= top && y <= bottom
}

/// Creates the card's model and registers `threadPanel.toggle`, `script.<id>.run` and
/// `editor.openFavorite`.
pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<Details> {
    let git = crate::git::init(state.clone(), cx);
    let details_state = state.clone();
    let details = cx.new(|cx| {
        let mut details = Details::new(details_state, git, cx);
        details.observe_git(cx);
        details
    });
    cx.set_global(DetailsGlobal(details.clone()));
    details.update(cx, |details, cx| details.start_remote_editor_probe(cx));
    let toggle = (state.clone(), details.clone());
    cx.on_action(move |_: &ToggleThreadPanel, cx| {
        let Some(key) = panel_key(toggle.0.read(cx)) else { return };
        toggle.1.update(cx, |details, cx| details.toggle(&key, cx));
    });
    let run = (state.clone(), details.clone());
    cx.on_action(move |action: &RunProjectScript, cx| {
        let Some((project_id, scripts)) = project_scripts(run.0.read(cx)) else { return };
        if let Some(script) = scripts.iter().find(|script| script.id == action.script_id) {
            run.1.update(cx, |details, cx| details.run_script(&project_id, script, cx));
        }
    });
    let favorite = (state, details.clone());
    cx.on_action(move |_: &OpenFavoriteEditor, cx| {
        let app = favorite.0.read(cx);
        let project_id = app
            .selected_thread
            .as_deref()
            .and_then(|id| app.thread_shell(id))
            .map(|thread| thread.project_id.as_str())
            .or(app.draft_project.as_deref());
        let has_active_project = project_id.and_then(|id| app.shell.project(id)).is_some();
        if !app.should_show_open_in_picker(has_active_project) {
            return;
        }
        let available = app.active_open_in_config().map(|config| config.available_editors.clone()).unwrap_or_default();
        let remote_state = app.active_remote_open_state();
        let can_open = match remote_state {
            t3_core::thread_details::RemoteOpenState::RemoteUnavailable => false,
            t3_core::thread_details::RemoteOpenState::RemoteLinks(_) => true,
            t3_core::thread_details::RemoteOpenState::LocalExec => favorite.1.read(cx).git.read(cx).can_operate_thread(),
        };
        if !can_open {
            return;
        }
        let effective = t3_core::thread_details::effective_open_in_editors(&remote_state, &available, &favorite.1.read(cx).remote_capable_editors());
        let editor = favorite.1.read(cx).preferred_editor(&effective);
        if let Some(editor) = editor {
            let state = favorite.0.clone();
            favorite.1.update(cx, |details, cx| details.open_in_editor(&state, editor, cx));
        }
    });
    details
}

/// The Run button's script for the open thread's project.
pub fn run_button_script<'a>(details: &Details, project_id: &str, scripts: &'a [ProjectScript]) -> Option<&'a ProjectScript> {
    preferred_script(scripts, details.last_script(project_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_goes_inline_once_the_lane_keeps_640px_beside_it() {
        assert_eq!(presentation_for(1011.), Presentation::Popover);
        assert_eq!(presentation_for(1012.), Presentation::Inline);
    }

    #[test]
    fn lane_shifts_left_only_as_far_as_the_card_needs() {
        assert_eq!(lane_inset_for(1500.), 0.);
        assert_eq!(lane_inset_for(1384.), 0.);
        assert_eq!(lane_inset_for(1200.), 184.);
        assert_eq!(lane_inset_for(1012.), 276.);
    }

    #[test]
    fn legacy_usage_flag_respects_reset_and_client_settings() {
        assert_eq!(legacy_usage_flag(false, Some(r#"{"threadDetailsUsageEnabled":true}"#)), Some(true));
        assert_eq!(legacy_usage_flag(true, Some(r#"{"threadDetailsUsageEnabled":true}"#)), None);
        assert_eq!(legacy_usage_flag(false, Some("not-json")), None);
    }

    #[test]
    fn clearing_popovers_preserves_inline_visibility() {
        let mut panels = HashMap::from([
            ("t1".to_owned(), PanelVisibility { inline_open: false, popover_open: true }),
            ("t2".to_owned(), PanelVisibility { inline_open: true, popover_open: true }),
        ]);
        clear_open_popovers(&mut panels);
        assert_eq!(panels["t1"], PanelVisibility { inline_open: false, popover_open: false });
        assert_eq!(panels["t2"], PanelVisibility { inline_open: true, popover_open: false });
    }

    #[test]
    fn available_height_subtracts_open_terminal_drawer() {
        assert_eq!(available_height_for(900., 0., 0.), 800.);
        assert_eq!(available_height_for(900., 280., 0.), 520.);
        assert_eq!(available_height_for(900., 0., 48.), 752.);
        assert_eq!(available_height_for(200., 280., 0.), 0.);
    }

    #[test]
    fn panel_key_covers_selected_threads_and_project_drafts() {
        assert_eq!(panel_key_for(Some("thread-1"), Some("proj-1")), Some("thread-1".to_owned()));
        assert_eq!(panel_key_for(None, Some("proj-1")), Some("draft:proj-1".to_owned()));
        assert_eq!(panel_key_for(None, None), None);
    }
}
