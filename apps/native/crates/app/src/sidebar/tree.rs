//! Keyboard focus for the virtual sidebar list (web/src/lib/sidebarTree.ts).

use std::collections::HashMap;

use gpui::{App, Context, FocusHandle, KeyDownEvent, Window};
use t3_core::sidebar::fleet::{ScopedRowKey, ScopedSidebarRow};
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::sidebar::{RowKey, Section, Shelf, SidebarRow};

use super::{PaneRowKey, SidebarPane};

#[derive(PartialEq, Eq, Hash)]
pub(super) enum TreeRow {
    Draft(String),
    Thread(ThreadKey),
    Folder(String),
    Shelf(Shelf),
    ShowMore,
}

impl TreeRow {
    pub(super) fn from_key(key: &PaneRowKey) -> Option<Self> {
        Some(match key {
            PaneRowKey::Draft(project) => Self::Draft(project.clone()),
            PaneRowKey::Thread(RowKey::Thread { id, .. }) => Self::Thread(ThreadKey::new("", id)),
            PaneRowKey::Scoped(ScopedRowKey::Thread { key, .. }) => Self::Thread(key.clone()),
            PaneRowKey::Scoped(ScopedRowKey::Folder(key)) => Self::Folder(key.clone()),
            PaneRowKey::Thread(RowKey::Shelf(shelf))
            | PaneRowKey::Scoped(ScopedRowKey::Shelf(shelf)) => Self::Shelf(*shelf),
            PaneRowKey::Thread(RowKey::ShowMore) | PaneRowKey::Scoped(ScopedRowKey::ShowMore) => {
                Self::ShowMore
            }
            _ => return None,
        })
    }
}

#[derive(Default)]
pub(super) struct TreeNavigation {
    handles: HashMap<TreeRow, FocusHandle>,
    visible: Vec<(usize, FocusHandle)>,
}

impl TreeNavigation {
    pub(super) fn sync(&mut self, rows: &[PaneRowKey], cx: &App) {
        let mut previous = std::mem::take(&mut self.handles);
        self.visible.clear();
        for (index, key) in rows.iter().enumerate() {
            if let Some(row) = TreeRow::from_key(key) {
                let handle = previous
                    .remove(&row)
                    .unwrap_or_else(|| cx.focus_handle().tab_stop(true));
                self.visible.push((index, handle.clone()));
                self.handles.insert(row, handle);
            }
        }
    }

    pub(super) fn focus(&self, row: &TreeRow) -> Option<FocusHandle> {
        self.handles.get(row).cloned()
    }

    pub(super) fn focus_for_row(&self, key: &PaneRowKey) -> Option<FocusHandle> {
        self.focus(&TreeRow::from_key(key)?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Folder,
    Shelf(Shelf),
}

#[derive(Default)]
struct Hierarchy {
    container: Option<(Container, bool)>,
    parent: Option<Container>,
}

impl Hierarchy {
    fn thread(section: Section, in_folder: bool) -> Self {
        let parent = match section {
            Section::Working => Some(Container::Shelf(Shelf::Working)),
            Section::Snoozed => Some(Container::Shelf(Shelf::Snoozed)),
            Section::Settled => Some(Container::Shelf(Shelf::Settled)),
            Section::Active if in_folder => Some(Container::Folder),
            _ => None,
        };
        Self {
            parent,
            ..Self::default()
        }
    }

    fn container(kind: Container, expanded: bool) -> Self {
        Self {
            container: Some((kind, expanded)),
            ..Self::default()
        }
    }

    fn show_more() -> Self {
        Self {
            parent: Some(Container::Shelf(Shelf::Settled)),
            ..Self::default()
        }
    }
}

impl SidebarPane {
    fn hierarchy(&self, index: usize, cx: &App) -> Hierarchy {
        let draft_count = self.drafts.rows.len();
        let Some(model_index) = index.checked_sub(draft_count + usize::from(draft_count > 0))
        else {
            return Hierarchy::default();
        };
        let state = self.state.read(cx);
        if state.fleet.environments.is_empty() {
            return match state.sidebar.model.rows.get(model_index) {
                Some(SidebarRow::Thread { section, .. }) => Hierarchy::thread(*section, false),
                Some(SidebarRow::Shelf {
                    shelf, expanded, ..
                }) => Hierarchy::container(Container::Shelf(*shelf), *expanded),
                Some(SidebarRow::ShowMore { .. }) => Hierarchy::show_more(),
                _ => Hierarchy::default(),
            };
        }
        match state.sidebar.fleet.rows.get(model_index) {
            Some(ScopedSidebarRow::Thread {
                section, in_folder, ..
            }) => Hierarchy::thread(*section, *in_folder),
            Some(ScopedSidebarRow::Shelf {
                shelf, expanded, ..
            }) => Hierarchy::container(Container::Shelf(*shelf), *expanded),
            Some(ScopedSidebarRow::Folder { expanded, .. }) => {
                Hierarchy::container(Container::Folder, *expanded)
            }
            Some(ScopedSidebarRow::ShowMore { .. }) => Hierarchy::show_more(),
            _ => Hierarchy::default(),
        }
    }

    pub(super) fn toggle_project_folder(
        &mut self,
        group: &t3_core::sidebar::grouping::ScopedProjectGroup,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.project_preferences.set_expanded(group, expanded);
            state.save_project_preferences(cx);
            state.rebuild_sidebar(cx);
            cx.emit(crate::app_state::AppEvent::Shell);
        });
    }

    fn toggle_tree_row(
        &mut self,
        index: usize,
        container: Container,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        match container {
            Container::Shelf(shelf) => self
                .state
                .update(cx, |state, cx| state.toggle_shelf(shelf, cx)),
            Container::Folder => {
                let Some(PaneRowKey::Scoped(ScopedRowKey::Folder(key))) = self.row_keys.get(index)
                else {
                    return;
                };
                if let Some(group) = self
                    .state
                    .read(cx)
                    .sidebar
                    .fleet
                    .folder_groups
                    .get(key)
                    .cloned()
                {
                    self.toggle_project_folder(&group, !expanded, cx);
                }
            }
        }
    }

    pub(super) fn navigate_tree(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if stroke.modifiers.modified()
            || !matches!(
                stroke.key.as_str(),
                "up" | "down" | "home" | "end" | "left" | "right"
            )
        {
            return;
        }
        let visible = &self.tree.visible;
        let Some(current) = visible
            .iter()
            .position(|(_, focus)| focus.is_focused(window))
        else {
            return;
        };
        window.prevent_default();
        cx.stop_propagation();
        let index = visible[current].0;
        let hierarchy = self.hierarchy(index, cx);
        let target = match stroke.key.as_str() {
            "up" => Some(current.saturating_sub(1)),
            "down" => Some((current + 1).min(visible.len() - 1)),
            "home" => Some(0),
            "end" => Some(visible.len() - 1),
            "left" => {
                if let Some((container, true)) = hierarchy.container {
                    self.toggle_tree_row(index, container, true, cx);
                    return;
                }
                hierarchy.parent.and_then(|parent| {
                    visible[..current].iter().rposition(|(index, _)| {
                        self.hierarchy(*index, cx)
                            .container
                            .is_some_and(|(container, _)| container == parent)
                    })
                })
            }
            "right" => match hierarchy.container {
                Some((container, false)) => {
                    self.toggle_tree_row(index, container, false, cx);
                    return;
                }
                Some((container, true)) => visible
                    .get(current + 1)
                    .filter(|(index, _)| self.hierarchy(*index, cx).parent == Some(container))
                    .map(|_| current + 1),
                None => None,
            },
            _ => None,
        };
        if let Some(target) = target {
            let (index, focus) = &visible[target];
            if stroke.key == "end" {
                self.list.scroll_to_end();
            } else {
                self.list.scroll_to_reveal_item(*index);
            }
            window.focus(focus, cx);
            cx.notify();
        }
    }
}

pub(super) fn context_menu_key(stroke: &gpui::Keystroke) -> bool {
    let modifiers = stroke.modifiers;
    !modifiers.control
        && !modifiers.alt
        && !modifiers.platform
        && !modifiers.function
        && ((stroke.key == "menu" && !modifiers.shift) || (stroke.key == "f10" && modifiers.shift))
}
