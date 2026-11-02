//! Per-thread right panel state: which surfaces (tabs) are open, which is active, whether the
//! panel shows, and the closed-tab history `cmd-shift-t` reopens. Ports `apps/web/src/rightPanelStore.ts`
//! (`upsertSurface`, `toggle`, `closeSurface`, `closeOtherSurfaces`, `closeSurfacesToRight`,
//! `closeAllSurfaces`, `moveSurface`, `setMaximized`), `apps/web/src/closedViewStore.ts` and
//! `planNextReopen` from `apps/web/src/reopenClosedView.ts`. Surfaces other than the diff plug in
//! as new `Surface` variants.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// `RightPanelSurface`: a tab. Singletons such as the diff use their kind as the id; surfaces
/// that can repeat (files, browsers, terminals) will carry their resource in the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    pub id: String,
    #[serde(flatten)]
    pub kind: SurfaceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SurfaceKind {
    Diff,
}

impl Surface {
    pub fn diff() -> Surface {
        Surface { id: "diff".to_owned(), kind: SurfaceKind::Diff }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `surfaceTitle` in `RightPanelTabs.tsx`.
    pub fn title(&self) -> &'static str {
        match self.kind {
            SurfaceKind::Diff => "Diff",
        }
    }
}

/// `ThreadRightPanelState`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPanel {
    pub is_open: bool,
    pub active_surface_id: Option<String>,
    pub surfaces: Vec<Surface>,
    pub maximized: bool,
}

impl ThreadPanel {
    /// The persisted-state migration in `rightPanelStore.ts`: surfaces this build does not know
    /// are dropped, and the active tab and visibility are repaired to match what is left.
    fn from_value(value: &Value) -> ThreadPanel {
        let surfaces: Vec<Surface> = value
            .get("surfaces")
            .and_then(Value::as_array)
            .map(|surfaces| surfaces.iter().filter_map(|surface| serde_json::from_value(surface.clone()).ok()).collect())
            .unwrap_or_default();
        let raw_active = value.get("activeSurfaceId").and_then(Value::as_str);
        let persisted_active = raw_active.filter(|id| surfaces.iter().any(|surface| surface.id == *id)).map(str::to_owned);
        let is_open = !surfaces.is_empty()
            && value.get("isOpen").and_then(Value::as_bool).unwrap_or(persisted_active.is_some());
        let active_surface_id = persisted_active.or_else(|| if is_open { surfaces.first().map(|surface| surface.id.clone()) } else { None });
        ThreadPanel {
            is_open,
            active_surface_id,
            surfaces,
            maximized: value.get("maximized").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

/// A map whose entries decode one by one: a bad entry is dropped, the rest survive.
pub fn decode_lenient_map<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> HashMap<String, T> {
    value
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(key, entry)| serde_json::from_value(entry.clone()).ok().map(|entry| (key.clone(), entry)))
                .collect()
        })
        .unwrap_or_default()
}

impl ThreadPanel {
    pub fn active(&self) -> Option<&Surface> {
        let id = self.active_surface_id.as_deref()?;
        self.surfaces.iter().find(|surface| surface.id() == id)
    }

    fn is_empty(&self) -> bool {
        !self.is_open && self.active_surface_id.is_none() && self.surfaces.is_empty() && !self.maximized
    }

    /// `upsertSurface`: adds the surface if missing, opens the panel, activates it.
    fn upsert(&mut self, surface: Surface) {
        self.is_open = true;
        self.active_surface_id = Some(surface.id().to_owned());
        if !self.surfaces.iter().any(|entry| entry.id == surface.id) {
            self.surfaces.push(surface);
        }
    }
}

/// `ClosedViewEntry` of kind `panel-tab`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedView {
    pub thread_id: String,
    pub surface: Surface,
}

const CLOSED_VIEW_LIMIT: usize = 20;

/// `usePreviewPanelInlineSize`: the inline panel's default and minimum width.
pub const DEFAULT_WIDTH: f32 = 540.;
pub const MIN_WIDTH: f32 = 360.;
/// The thread column beside the panel keeps at least this much, or its composer overflows.
const SIBLING_COLUMN_MIN_WIDTH: f32 = 360.;

/// `getPreviewPanelMaxWidth`: 70% of the window and whatever leaves the thread its minimum,
/// never below the minimum width (an inverted clamp would overwrite the saved width).
pub fn clamp_width(width: f32, viewport_width: f32, row_width: f32) -> f32 {
    let max = (viewport_width * 0.7).floor().min(row_width.floor() - SIBLING_COLUMN_MIN_WIDTH).max(MIN_WIDTH);
    width.clamp(MIN_WIDTH, max)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RightPanels {
    threads: HashMap<String, ThreadPanel>,
    /// `t3code:preview-panel-width:<thread>`.
    widths: HashMap<String, f32>,
    /// Newest first.
    closed: Vec<ClosedView>,
}

impl RightPanels {
    /// Decodes persisted state without trusting it: every part that fails to decode falls back
    /// on its own, so one bad entry never costs the user their other tabs.
    pub fn from_value(value: &Value) -> RightPanels {
        let threads = value
            .get("threads")
            .and_then(Value::as_object)
            .map(|threads| {
                threads
                    .iter()
                    .map(|(thread, panel)| (thread.clone(), ThreadPanel::from_value(panel)))
                    .filter(|(_, panel)| !panel.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let closed = value
            .get("closed")
            .and_then(Value::as_array)
            .map(|closed| closed.iter().filter_map(|view| serde_json::from_value(view.clone()).ok()).take(CLOSED_VIEW_LIMIT).collect())
            .unwrap_or_default();
        let widths = decode_lenient_map::<f32>(value.get("widths")).into_iter().filter(|(_, width)| width.is_finite()).collect();
        RightPanels { threads, widths, closed }
    }
}

impl<'de> Deserialize<'de> for RightPanels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(RightPanels::from_value(&Value::deserialize(deserializer)?))
    }
}

static EMPTY: ThreadPanel = ThreadPanel { is_open: false, active_surface_id: None, surfaces: Vec::new(), maximized: false };

impl RightPanels {
    pub fn panel(&self, thread_id: &str) -> &ThreadPanel {
        self.threads.get(thread_id).unwrap_or(&EMPTY)
    }

    /// `selectActiveRightPanelSurface`: the active surface while the panel shows.
    pub fn active_surface(&self, thread_id: &str) -> Option<&Surface> {
        let panel = self.panel(thread_id);
        if panel.is_open { panel.active() } else { None }
    }

    pub fn is_open(&self, thread_id: &str) -> bool {
        self.panel(thread_id).is_open
    }

    pub fn width(&self, thread_id: &str) -> Option<f32> {
        self.widths.get(thread_id).copied()
    }

    pub fn closed_views(&self) -> &[ClosedView] {
        &self.closed
    }

    pub fn set_width(&mut self, thread_id: &str, width: f32) -> bool {
        if self.widths.get(thread_id) == Some(&width) {
            return false;
        }
        self.widths.insert(thread_id.to_owned(), width);
        true
    }

    /// Applies `update` and drops the entry once it holds nothing, like `updateThreadStateMap`.
    fn update(&mut self, thread_id: &str, update: impl FnOnce(&mut ThreadPanel)) -> bool {
        let before = self.panel(thread_id).clone();
        let mut next = before.clone();
        update(&mut next);
        if next == before {
            return false;
        }
        if next.is_empty() {
            self.threads.remove(thread_id);
        } else {
            self.threads.insert(thread_id.to_owned(), next);
        }
        true
    }

    /// Remembers what a close removed: the tabs that were not active first, then the active one,
    /// so reopening brings back the tab the user was looking at first.
    fn close(&mut self, thread_id: &str, update: impl FnOnce(&mut ThreadPanel)) -> bool {
        let before = self.panel(thread_id).clone();
        if !self.update(thread_id, update) {
            return false;
        }
        let after = self.panel(thread_id).surfaces.clone();
        let active = before.active_surface_id.as_deref();
        let removed: Vec<&Surface> =
            before.surfaces.iter().filter(|surface| !after.iter().any(|entry| entry.id == surface.id)).collect();
        let ordered = removed
            .iter()
            .filter(|surface| Some(surface.id()) != active)
            .chain(removed.iter().filter(|surface| Some(surface.id()) == active));
        for surface in ordered {
            self.remember(ClosedView { thread_id: thread_id.to_owned(), surface: (*surface).clone() });
        }
        true
    }

    /// `closedViewStore.remember`: newest first, one entry per target, at most 20.
    fn remember(&mut self, view: ClosedView) {
        self.closed.retain(|entry| entry.thread_id != view.thread_id || entry.surface.id != view.surface.id);
        self.closed.insert(0, view);
        self.closed.truncate(CLOSED_VIEW_LIMIT);
    }

    pub fn open(&mut self, thread_id: &str, surface: Surface) -> bool {
        self.update(thread_id, |panel| panel.upsert(surface))
    }

    /// `toggle`: hides the panel when the surface is already in front, otherwise opens it.
    pub fn toggle(&mut self, thread_id: &str, surface: Surface) -> bool {
        self.update(thread_id, |panel| {
            if panel.is_open && panel.active().is_some_and(|active| active.id == surface.id) {
                panel.is_open = false;
            } else {
                panel.upsert(surface);
            }
        })
    }

    pub fn toggle_visibility(&mut self, thread_id: &str) -> bool {
        self.update(thread_id, |panel| panel.is_open = !panel.is_open)
    }

    pub fn hide(&mut self, thread_id: &str) -> bool {
        self.update(thread_id, |panel| panel.is_open = false)
    }

    pub fn activate(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.update(thread_id, |panel| {
            if panel.surfaces.iter().any(|surface| surface.id() == surface_id) {
                panel.is_open = true;
                panel.active_surface_id = Some(surface_id.to_owned());
            }
        })
    }

    /// `closeSurface`: the tab at the same position, or the new last one, becomes active.
    pub fn close_surface(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            panel.surfaces.remove(index);
            panel.is_open = panel.is_open && !panel.surfaces.is_empty();
            if panel.active_surface_id.as_deref() == Some(surface_id) {
                let fallback = panel.surfaces.get(index.min(panel.surfaces.len().saturating_sub(1)));
                panel.active_surface_id = fallback.map(|surface| surface.id().to_owned());
            }
        })
    }

    pub fn close_other_surfaces(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(surface) = panel.surfaces.iter().find(|surface| surface.id() == surface_id).cloned() else { return };
            if panel.surfaces.len() == 1 {
                return;
            }
            panel.is_open = true;
            panel.active_surface_id = Some(surface.id().to_owned());
            panel.surfaces = vec![surface];
        })
    }

    pub fn close_surfaces_to_right(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            panel.surfaces.truncate(index + 1);
            let active_kept = panel.active().is_some();
            if !active_kept {
                panel.active_surface_id = Some(surface_id.to_owned());
            }
        })
    }

    pub fn close_all_surfaces(&mut self, thread_id: &str) -> bool {
        self.close(thread_id, |panel| {
            if !panel.surfaces.is_empty() {
                panel.is_open = false;
                panel.surfaces.clear();
                panel.active_surface_id = None;
            }
        })
    }

    pub fn move_surface(&mut self, thread_id: &str, surface_id: &str, to_index: usize) -> bool {
        self.update(thread_id, |panel| {
            let Some(from) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            let surface = panel.surfaces.remove(from);
            panel.surfaces.insert(to_index.min(panel.surfaces.len()), surface);
        })
    }

    pub fn set_maximized(&mut self, thread_id: &str, maximized: bool) -> bool {
        self.update(thread_id, |panel| panel.maximized = maximized)
    }

    /// `planNextReopen` + `reopenClosedView`: restores the newest closed tab whose thread still
    /// exists and is not already showing it. Returns that thread, which the caller selects.
    pub fn reopen_closed(&mut self, thread_exists: impl Fn(&str) -> bool) -> Option<String> {
        let mut index = 0;
        while index < self.closed.len() {
            let entry = &self.closed[index];
            if !thread_exists(&entry.thread_id) {
                index += 1;
                continue;
            }
            let panel = self.panel(&entry.thread_id);
            if panel.is_open && panel.surfaces.iter().any(|surface| surface.id == entry.surface.id) {
                self.closed.remove(index);
                continue;
            }
            let entry = self.closed.remove(index);
            self.open(&entry.thread_id, entry.surface);
            return Some(entry.thread_id);
        }
        None
    }

    pub fn remove_thread(&mut self, thread_id: &str) -> bool {
        let removed = self.threads.remove(thread_id).is_some();
        self.widths.remove(thread_id).is_some() || removed
    }
}

#[cfg(test)]
mod tests;
