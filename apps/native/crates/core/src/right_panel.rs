//! Per-thread right panel state: which surfaces (tabs) are open, which is active, whether the
//! panel shows, and the closed-tab history `cmd-shift-t` reopens. Ports `apps/web/src/rightPanelStore.ts`
//! (`upsertSurface`, `toggle`, `closeSurface`, `closeOtherSurfaces`, `closeSurfacesToRight`,
//! `closeAllSurfaces`, `moveSurface`, `setMaximized`), `apps/web/src/closedViewStore.ts` and
//! `planNextReopen` from `apps/web/src/reopenClosedView.ts`, with the file surfaces' `openFile` and
//! `navigateFolder`, the browser tabs' `openBrowser` and `reconcileBrowserSurfaces`, and the device
//! tabs' `openDevice`. Other surfaces plug in as new `Surface` variants.

use std::collections::{HashMap, HashSet};

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
    PullRequests,
    /// The workspace explorer on its own.
    Files,
    /// A file (or, until its read says otherwise, a folder) at a workspace-relative or host path.
    /// `reveal_request_id` grows on every open so opening the same line again scrolls again.
    #[serde(rename_all = "camelCase")]
    File {
        relative_path: String,
        #[serde(default, deserialize_with = "reveal_line")]
        reveal_line: Option<u64>,
        #[serde(default)]
        reveal_request_id: u64,
    },
    #[serde(rename_all = "camelCase")]
    SideChat { thread_id: String },
    /// A browser tab: `browser:<tab>` for a preview session, `browser:new` before one opens.
    #[serde(rename_all = "camelCase")]
    Preview { resource_id: Option<String> },
    /// A device tab: `device` while it lists devices, `device:<host>:<device>` once one opened.
    Device {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<DeviceTarget>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

/// `DeviceTabTarget`: the device a tab shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTarget {
    pub host_id: String,
    pub device_id: String,
    pub platform: crate::device::Platform,
    pub name: String,
}

/// `normalizeRevealLine`: a positive whole line, anything else none.
fn reveal_line<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?.and_then(|value| value.as_f64()).filter(|line| line.is_finite()).map(|line| line.trunc().max(1.) as u64))
}

impl Surface {
    pub fn diff() -> Surface {
        Surface { id: "diff".to_owned(), kind: SurfaceKind::Diff }
    }

    /// A thread's linked pull requests (`{ id: "pull-requests", kind: "pull-requests" }`).
    pub fn pull_requests() -> Surface {
        Surface { id: "pull-requests".to_owned(), kind: SurfaceKind::PullRequests }
    }

    pub fn files() -> Surface {
        Surface { id: "files".to_owned(), kind: SurfaceKind::Files }
    }

    pub fn file(relative_path: &str, reveal_line: Option<u64>, reveal_request_id: u64) -> Surface {
        Surface {
            id: format!("file:{relative_path}"),
            kind: SurfaceKind::File { relative_path: relative_path.to_owned(), reveal_line, reveal_request_id },
        }
    }

    /// `side-chat:<thread>`, as `openSideChat` keys it.
    pub fn side_chat(thread_id: &str) -> Surface {
        Surface { id: format!("side-chat:{thread_id}"), kind: SurfaceKind::SideChat { thread_id: thread_id.to_owned() } }
    }

    /// `browserSurface`.
    pub fn browser(tab_id: Option<&str>) -> Surface {
        match tab_id {
            Some(tab_id) => Surface { id: format!("browser:{tab_id}"), kind: SurfaceKind::Preview { resource_id: Some(tab_id.to_owned()) } },
            None => Surface { id: BROWSER_PLACEHOLDER_ID.to_owned(), kind: SurfaceKind::Preview { resource_id: None } },
        }
    }

    /// The device tab: the `device` placeholder, or the one keyed by its device.
    pub fn device(target: Option<DeviceTarget>) -> Surface {
        let id = match &target {
            Some(target) => format!("device:{}:{}", crate::device::encode_component(&target.host_id), crate::device::encode_component(&target.device_id)),
            None => DEVICE_PLACEHOLDER_ID.to_owned(),
        };
        Surface { id, kind: SurfaceKind::Device { target, title: None } }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The preview session a browser tab shows.
    pub fn preview_tab(&self) -> Option<&str> {
        match &self.kind {
            SurfaceKind::Preview { resource_id } => resource_id.as_deref(),
            _ => None,
        }
    }

    /// `surfaceTitle` in `RightPanelTabs.tsx`: a file tab shows its name.
    pub fn title(&self) -> String {
        match &self.kind {
            SurfaceKind::Diff => "Diff".to_owned(),
            SurfaceKind::PullRequests => "Pull requests".to_owned(),
            SurfaceKind::Files => "Files".to_owned(),
            SurfaceKind::File { relative_path, .. } => {
                let start = relative_path.rfind(['/', '\\']).map_or(0, |index| index + 1);
                relative_path[start..].to_owned()
            }
            SurfaceKind::SideChat { .. } => "Side chat".to_owned(),
            SurfaceKind::Preview { .. } => "Browser".to_owned(),
            SurfaceKind::Device { target, title } => title.clone().or_else(|| target.as_ref().map(|target| target.name.clone())).unwrap_or_else(|| "Device".to_owned()),
        }
    }

    pub fn side_chat_thread(&self) -> Option<&str> {
        match &self.kind {
            SurfaceKind::SideChat { thread_id } => Some(thread_id),
            _ => None,
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
const BROWSER_PLACEHOLDER_ID: &str = "browser:new";
const DEVICE_PLACEHOLDER_ID: &str = "device";

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

    /// The thread the visible panel needs beside the selected thread.
    pub fn active_thread_source<'a>(&'a self, thread_id: &str, selection: &'a crate::diff::DiffSelection) -> Option<&'a str> {
        match &self.active_surface(thread_id)?.kind {
            SurfaceKind::SideChat { thread_id } => Some(thread_id),
            SurfaceKind::Diff => match selection {
                crate::diff::DiffSelection::ToolCall(target) => Some(&target.thread_id),
                _ => None,
            },
            _ => None,
        }
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
        // A closed browser tab's session closes with it, so there is nothing to reopen by id.
        for surface in ordered.filter(|surface| surface.preview_tab().is_none()) {
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

    /// `show`: the panel opens, on the launcher when it has no tab.
    pub fn show(&mut self, thread_id: &str) -> bool {
        self.update(thread_id, |panel| panel.is_open = true)
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

    /// `openFile`: `None` opens the explorer; a path replaces the standalone explorer with its
    /// file tab (which carries the explorer beside it) and asks for a fresh reveal.
    pub fn open_file(&mut self, thread_id: &str, path: Option<&str>, line: Option<u64>) -> bool {
        let Some(path) = path else { return self.open(thread_id, Surface::files()) };
        self.update(thread_id, |panel| {
            panel.surfaces.retain(|surface| surface.kind != SurfaceKind::Files);
            let id = format!("file:{path}");
            let previous = panel.surfaces.iter().find_map(|surface| match &surface.kind {
                SurfaceKind::File { reveal_request_id, .. } if surface.id == id => Some(*reveal_request_id),
                _ => None,
            });
            let surface = Surface::file(path, line.map(|line| line.max(1)), previous.unwrap_or(0) + 1);
            panel.is_open = true;
            panel.active_surface_id = Some(id.clone());
            match panel.surfaces.iter_mut().find(|entry| entry.id == id) {
                Some(existing) => *existing = surface,
                None => panel.surfaces.push(surface),
            }
        })
    }

    /// `navigateFolder` without the breadcrumb trail: replaces `surface_id` with the folder
    /// (`""` is the explorer), or opens it beside the source when `keep_source`.
    pub fn navigate_folder(&mut self, thread_id: &str, surface_id: &str, folder: &str, keep_source: bool) -> bool {
        self.update(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id == surface_id) else { return };
            let target = if folder.is_empty() { Surface::files() } else { Surface::file(folder, None, 1) };
            let exists = panel.surfaces.iter().enumerate().any(|(i, surface)| i != index && surface.id == target.id);
            let source = panel.surfaces[index].clone();
            let mut surfaces = Vec::with_capacity(panel.surfaces.len() + 1);
            for (i, surface) in panel.surfaces.drain(..).enumerate() {
                if i == index {
                    if keep_source {
                        surfaces.push(source.clone());
                    }
                    if !exists {
                        surfaces.push(target.clone());
                    }
                } else if exists || !matches!(target.kind, SurfaceKind::File { .. }) || surface.kind != SurfaceKind::Files {
                    surfaces.push(surface);
                }
            }
            panel.surfaces = surfaces;
            panel.is_open = true;
            panel.active_surface_id = Some(target.id);
        })
    }

    /// `reconcileFileSurfaces`: without a workspace, file tabs and the explorer close.
    pub fn close_file_surfaces(&mut self, thread_id: &str) -> bool {
        self.update(thread_id, |panel| {
            let before = panel.surfaces.len();
            panel.surfaces.retain(|surface| !matches!(surface.kind, SurfaceKind::Files | SurfaceKind::File { .. }));
            if panel.surfaces.len() == before {
                return;
            }
            if panel.surfaces.is_empty() {
                panel.is_open = false;
            }
            if panel.active().is_none() {
                panel.active_surface_id = panel.surfaces.last().map(|surface| surface.id.clone());
            }
        })
    }

    /// `openBrowser`: a session's tab replaces the `browser:new` placeholder.
    pub fn open_browser(&mut self, thread_id: &str, tab_id: Option<&str>) -> bool {
        self.update(thread_id, |panel| {
            if tab_id.is_some() {
                panel.surfaces.retain(|surface| surface.id != BROWSER_PLACEHOLDER_ID);
            }
            panel.upsert(Surface::browser(tab_id));
        })
    }

    /// `openDevice`: an opened device's tab takes the `device` placeholder's place, or, when the
    /// device already has a tab, the placeholder goes and that tab comes to the front.
    pub fn open_device(&mut self, thread_id: &str, target: DeviceTarget) -> bool {
        let surface = Surface::device(Some(target));
        self.update(thread_id, |panel| {
            let existing = panel.surfaces.iter().any(|entry| entry.id == surface.id);
            if existing {
                panel.surfaces.retain(|entry| entry.id != DEVICE_PLACEHOLDER_ID);
            } else if let Some(placeholder) = panel.surfaces.iter_mut().find(|entry| entry.id == DEVICE_PLACEHOLDER_ID) {
                *placeholder = surface.clone();
            }
            panel.upsert(surface);
        })
    }

    /// `reconcileBrowserSurfaces`: browser tabs follow the thread's preview sessions. Closed
    /// sessions lose their tab, new ones gain one unless `hidden`, and the placeholder goes.
    pub fn reconcile_browser_surfaces(&mut self, thread_id: &str, tab_ids: &[String], hidden: &HashSet<String>) -> bool {
        self.update(thread_id, |panel| {
            let had_session_tab = panel.surfaces.iter().any(|surface| surface.preview_tab().is_some());
            panel.surfaces.retain(|surface| match &surface.kind {
                SurfaceKind::Preview { resource_id: Some(tab) } => tab_ids.contains(tab),
                SurfaceKind::Preview { resource_id: None } => false,
                _ => true,
            });
            for tab in tab_ids {
                if !hidden.contains(tab) && !panel.surfaces.iter().any(|surface| surface.preview_tab() == Some(tab.as_str())) {
                    panel.surfaces.push(Surface::browser(Some(tab)));
                }
            }
            // The last session tab closed: hide the panel as a user close would.
            if panel.surfaces.is_empty() && had_session_tab {
                panel.is_open = false;
            }
            if panel.active().is_none() {
                let fallback = panel.surfaces.iter().find(|surface| matches!(surface.kind, SurfaceKind::Preview { .. })).or(panel.surfaces.first());
                panel.active_surface_id = fallback.map(|surface| surface.id.clone());
            }
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
            // A closed side chat tab the server since deleted is skipped like a deleted thread.
            if !thread_exists(&entry.thread_id) || entry.surface.side_chat_thread().is_some_and(|id| !thread_exists(id)) {
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
