//! Thread details (ledger TV-DETAILS): the 280px card of workspace, version control and lineage
//! controls beside the transcript. Ports `components/chat/ThreadDetailsPanel.tsx`,
//! `ThreadDetailsCard.tsx`, `threadDetailsCardLayout.ts`, the card's per-thread visibility in
//! `rightPanelStore.ts`, and `threadPanel.toggle`. Inline at the canvas's top-right when the
//! transcript lane still fits beside it, else a popover under the header's toggle.

pub(crate) mod controls;
mod items;
mod sections;
mod view;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Action, App, AppContext, Context, Entity, EventEmitter, Global, Window, actions};
use serde::{Deserialize, Serialize};
use t3_client::store;
use t3_core::project_scripts::{ProjectScript, preferred_script, resolve_project_scripts};
use t3_core::right_panel::{DEFAULT_WIDTH, clamp_width};
use t3_core::server_config::ServerSettings;
use t3_core::vcs;

pub use view::DetailsView;

use crate::app_state::AppState;
use crate::git::GitStore;
use crate::theme::metrics;
use crate::workspace::SIDEBAR_WIDTH;

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
    last_scripts: HashMap<String, String>,
    last_editor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsEvent {
    Changed,
}

/// The card's preferences and the open thread's Git store.
pub struct Details {
    pub git: Entity<GitStore>,
    panels: HashMap<String, PanelVisibility>,
    pub items: items::Visibility,
    last_scripts: HashMap<String, String>,
    last_editor: Option<String>,
    /// The mode the card last rendered in, so the toggle acts on what is on screen.
    presentation: Presentation,
    /// A press outside closes the popover just before the toggle's click would reopen it.
    popover_closed_at: Option<Instant>,
}

impl EventEmitter<DetailsEvent> for Details {}

struct DetailsGlobal(Entity<Details>);

impl Global for DetailsGlobal {}

pub fn details(cx: &App) -> Option<Entity<Details>> {
    cx.try_global::<DetailsGlobal>().map(|global| global.0.clone())
}

impl Details {
    fn new(git: Entity<GitStore>) -> Self {
        let persisted: Persisted = store::load_ui_state(STORE).and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
        Details {
            git,
            panels: persisted.panels,
            items: items::Visibility::from_overrides(persisted.items),
            last_scripts: persisted.last_scripts,
            last_editor: persisted.last_editor,
            presentation: Presentation::Inline,
            popover_closed_at: None,
        }
    }

    fn save(&self, cx: &mut Context<Self>) {
        let persisted = Persisted {
            panels: self.panels.iter().filter(|(_, visibility)| **visibility != PanelVisibility::default()).map(|(id, v)| (id.clone(), *v)).collect(),
            items: self.items.overrides().clone(),
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
        self.items.set(id, visible);
        self.save(cx);
    }

    pub fn reset_items(&mut self, cx: &mut Context<Self>) {
        self.items.reset();
        self.save(cx);
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
        if self.last_scripts.get(project_id) != Some(&script.id) {
            self.last_scripts.insert(project_id.to_owned(), script.id.clone());
            self.save(cx);
        }
        self.git.update(cx, |git, cx| git.run_script(script, cx));
    }

    pub fn open_in_editor(&mut self, editor: &str, cx: &mut Context<Self>) {
        self.set_last_editor(editor, cx);
        self.git.update(cx, |git, cx| git.open_in_editor(editor, cx));
    }
}

/// The open thread's effective project scripts: the shell's, as server settings override them
/// (`resolveProjectScripts` in `ChatView.tsx`).
pub fn project_scripts(state: &AppState) -> Option<(String, Vec<ProjectScript>)> {
    let thread = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id))?;
    let project = state.shell.project(&thread.project_id)?;
    let own = project.scripts.iter().filter_map(|value| serde_json::from_value(value.clone()).ok()).collect();
    let scripts = match state.server_config.config.as_ref() {
        Some(config) => resolve_project_scripts(&config.settings, &project.id, own),
        None => resolve_project_scripts(&ServerSettings::default(), &project.id, own),
    };
    Some((project.id.clone(), scripts))
}

/// How wide the chat canvas is: the window minus the sidebar and an inline right panel.
fn canvas_width(state: &AppState, viewport_width: f32) -> f32 {
    let sidebar = if state.sidebar.collapsed { 0. } else { SIDEBAR_WIDTH };
    let row = viewport_width - sidebar;
    let panel = state.selected_thread.as_deref().map_or(0., |thread| {
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
    let card_open = match (app.selected_thread.as_deref(), details(cx)) {
        (Some(thread), Some(details)) => details.read(cx).is_open(thread, presentation),
        _ => false,
    };
    let lane_inset = if card_open && presentation == Presentation::Inline { lane_inset_for(width) } else { 0. };
    Layout { presentation, card_open, lane_inset }
}

/// Creates the card's model and registers `threadPanel.toggle`, `script.<id>.run` and
/// `editor.openFavorite`.
pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<Details> {
    let git = crate::git::init(state.clone(), cx);
    let details = cx.new(|_| Details::new(git));
    cx.set_global(DetailsGlobal(details.clone()));
    let toggle = (state.clone(), details.clone());
    cx.on_action(move |_: &ToggleThreadPanel, cx| {
        let Some(thread) = toggle.0.read(cx).selected_thread.clone() else { return };
        toggle.1.update(cx, |details, cx| details.toggle(&thread, cx));
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
        let available = favorite.0.read(cx).server_config.config.as_ref().map(|config| config.available_editors.clone()).unwrap_or_default();
        let editor = favorite.1.read(cx).preferred_editor(&available);
        if let Some(editor) = editor {
            favorite.1.update(cx, |details, cx| details.open_in_editor(editor, cx));
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
}
