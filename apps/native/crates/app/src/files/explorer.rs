//! The explorer (`FileBrowserPanel.tsx`, `useDirectoryEntries.ts`): folders listed as they open,
//! at most four listings in flight, ignored entries flagged, the search field merging
//! `projects.searchEntries` results (`useProjectPathSearch`, debounced 200ms), refresh of what
//! was listed, reveal of the open file, and the keyboard model of Pierre's tree.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, KeyBinding, ScrollStrategy, Subscription, Task, UniformListScrollHandle,
    Window, actions,
};
use gpui_component::input::{InputEvent, InputState};
use t3_core::files::{
    Entry, EntryKind, FileTree, LIST_ENTRIES, SEARCH_ENTRIES, TreeRow, decode_listing, decode_search, list_entries_request,
    search_entries_request,
};

use super::Root;
use crate::app_state::AppState;

pub(super) const CONTEXT: &str = "FileTree";
/// `useDirectoryEntries` runs at most four listings at once.
const MAX_IN_FLIGHT: usize = 4;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

actions!(file_tree, [SelectPrevious, SelectNext, ExpandSelected, CollapseSelected, OpenSelected]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("right", ExpandSelected, Some(CONTEXT)),
        KeyBinding::new("left", CollapseSelected, Some(CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(CONTEXT)),
    ]);
}

#[derive(Default)]
pub(super) struct Search {
    pub(super) query: String,
    /// The query the results answer.
    answered: String,
    pub(super) found: Vec<Entry>,
    pub(super) truncated: bool,
    pub(super) pending: bool,
    pub(super) error: Option<String>,
    _task: Option<Task<()>>,
}

pub struct Explorer {
    pub(super) state: Entity<AppState>,
    pub(super) root: Option<Root>,
    /// Bumps with the root, so listings for a previous workspace are dropped.
    epoch: u64,
    pub(super) tree: FileTree,
    pub(super) rows: Vec<TreeRow>,
    rows_dirty: bool,
    queue: VecDeque<(String, u64)>,
    in_flight: usize,
    pub(super) search: Search,
    pub(super) search_input: Entity<InputState>,
    /// The open file, highlighted in the tree.
    pub(super) selected: Option<String>,
    /// The keyboard cursor.
    pub(super) cursor: Option<String>,
    /// A selection made in the tree: the reveal it causes must not close the search or scroll.
    tree_selection: Option<String>,
    handled_reveal: Option<(String, u64)>,
    pending_scroll: Option<String>,
    expanded_folder: Option<String>,
    /// A reveal closed the search; the field clears on the next render, which has the window.
    pub(super) pending_reset_input: bool,
    pub(super) scroll: UniformListScrollHandle,
    pub(super) focus: FocusHandle,
    /// Whether the tree had focus at the last render: the cursor row shows only then.
    pub(super) tree_focused: bool,
    _subscription: Subscription,
}

impl Explorer {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search files").clean_on_escape());
        let subscription = cx.subscribe(&search_input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                this.set_query(query, cx);
            }
        });
        Explorer {
            state,
            root: None,
            epoch: 0,
            tree: FileTree::default(),
            rows: Vec::new(),
            rows_dirty: false,
            queue: VecDeque::new(),
            in_flight: 0,
            search: Search::default(),
            search_input,
            selected: None,
            cursor: None,
            tree_selection: None,
            handled_reveal: None,
            pending_scroll: None,
            expanded_folder: None,
            pending_reset_input: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle().tab_stop(true),
            tree_focused: false,
            _subscription: subscription,
        }
    }

    pub fn set_root(&mut self, root: Option<Root>, cx: &mut Context<Self>) {
        if root == self.root {
            return;
        }
        self.root = root;
        self.epoch += 1;
        self.tree = FileTree::default();
        self.queue.clear();
        self.in_flight = 0;
        self.search = Search { query: self.search.query.clone(), ..Search::default() };
        self.handled_reveal = None;
        self.expanded_folder = None;
        self.cursor = None;
        self.mark_dirty();
        if self.root.is_some() {
            self.load(String::new(), false, cx);
            if !self.search.query.trim().is_empty() {
                self.schedule_search(cx);
            }
        }
        cx.notify();
    }

    pub(super) fn mark_dirty(&mut self) {
        self.rows_dirty = true;
    }

    /// Rows are rebuilt at most once per frame, however many listings landed.
    pub(super) fn sync_rows(&mut self) {
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        self.rows = if self.search.query.trim().is_empty() {
            self.tree.rows()
        } else {
            let found: &[Entry] = if self.search.pending { &[] } else { &self.search.found };
            self.tree.search_rows(&self.search.query, found)
        };
        if let Some(path) = self.pending_scroll.clone()
            && let Some(index) = self.rows.iter().position(|row| row.path == path)
        {
            self.pending_scroll = None;
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn load(&mut self, directory: String, refresh: bool, cx: &mut Context<Self>) {
        if let Some(generation) = self.tree.begin_load(&directory, refresh) {
            self.queue.push_back((directory, generation));
            self.mark_dirty();
            self.pump(cx);
        }
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        while self.in_flight < MAX_IN_FLIGHT {
            let Some((directory, generation)) = self.queue.pop_front() else { return };
            let Some(root) = self.root.clone() else { return };
            let payload = list_entries_request(&root.workspace.cwd, &directory);
            let decode_directory = directory.clone();
            let task = self.state.read(cx).request(LIST_ENTRIES, payload, move |value| decode_listing(&value, &decode_directory));
            let Some(task) = task else {
                self.tree.fail_load(&directory, generation, "Not connected to the server.".to_owned());
                self.mark_dirty();
                continue;
            };
            self.in_flight += 1;
            let epoch = self.epoch;
            cx.spawn(async move |this, cx| {
                let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
                let _ = this.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return;
                    }
                    this.in_flight = this.in_flight.saturating_sub(1);
                    match result {
                        Ok(listing) => {
                            for folder in this.tree.finish_load(&directory, generation, listing.entries) {
                                this.load(folder, false, cx);
                            }
                        }
                        Err(error) => {
                            let message = if error.trim().is_empty() { "Unable to load folder.".to_owned() } else { error };
                            this.tree.fail_load(&directory, generation, message);
                        }
                    }
                    this.mark_dirty();
                    this.pump(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Refresh: lists every listed folder again and reruns the search.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.root.is_none() {
            return;
        }
        for directory in self.tree.refresh_targets() {
            self.load(directory, true, cx);
        }
        if !self.tree.ready() && !self.tree.is_loading("") {
            self.load(String::new(), false, cx);
        }
        if !self.search.query.trim().is_empty() {
            self.search.answered.clear();
            self.schedule_search(cx);
        }
        cx.notify();
    }

    /// A new session is up: folders that failed while it was down are listed again.
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        for directory in self.tree.failed_directories() {
            self.load(directory, false, cx);
        }
        if !self.tree.ready() {
            self.load(String::new(), false, cx);
        }
        cx.notify();
    }

    pub fn is_refreshing(&self) -> bool {
        self.in_flight > 0 || !self.queue.is_empty() || self.search.pending
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.search.query {
            return;
        }
        self.search.query = query;
        self.mark_dirty();
        if self.search.query.trim().is_empty() {
            self.search = Search::default();
        } else {
            self.schedule_search(cx);
        }
        cx.notify();
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let query = self.search.query.trim().to_owned();
        if query == self.search.answered {
            return;
        }
        self.search.pending = true;
        self.search.error = None;
        self.search._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let Ok(Some(task)) = this.update(cx, |this, cx| {
                this.state.read(cx).request(SEARCH_ENTRIES, search_entries_request(&root.workspace.cwd, &query), |value| decode_search(&value))
            }) else {
                let _ = this.update(cx, |this, cx| {
                    this.search.pending = false;
                    this.search.error = Some("Not connected to the server.".to_owned());
                    cx.notify();
                });
                return;
            };
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.search.query.trim() != query {
                    return;
                }
                this.search.pending = false;
                this.search.answered = query;
                match result {
                    Ok(listing) => {
                        this.search.found = listing.entries;
                        this.search.truncated = listing.truncated;
                    }
                    Err(error) => this.search.error = Some(error),
                }
                this.mark_dirty();
                cx.notify();
            });
        }));
    }

    /// The open file changed. An outside selection (a link, a picker) reveals it: its folders
    /// open, the search closes, and it scrolls into the middle (`selectedPathRevealId`).
    pub fn set_selected(&mut self, path: Option<String>, reveal_request_id: u64, cx: &mut Context<Self>) {
        let changed = path != self.selected;
        self.selected = path.clone();
        let Some(path) = path else {
            self.handled_reveal = None;
            if changed {
                cx.notify();
            }
            return;
        };
        let request = (path.clone(), reveal_request_id);
        if self.handled_reveal.as_ref() == Some(&request) {
            return;
        }
        self.handled_reveal = Some(request);
        self.cursor = Some(path.clone());
        for directory in self.tree.reveal(&path) {
            self.load(directory, false, cx);
        }
        if self.tree_selection.take().as_deref() != Some(path.as_str()) {
            if !self.search.query.is_empty() {
                self.search = Search::default();
                self.pending_reset_input = true;
            }
            self.pending_scroll = Some(path);
        }
        self.mark_dirty();
        cx.notify();
    }

    /// The open path is a folder: it opens in the tree, once per path.
    pub fn expand_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.expanded_folder.as_deref() == Some(path) {
            return;
        }
        self.expanded_folder = Some(path.to_owned());
        if let Some(directory) = self.tree.expand(path) {
            self.load(directory, false, cx);
        }
        self.mark_dirty();
        cx.notify();
    }

    fn row_index(&self, path: Option<&str>) -> Option<usize> {
        let path = path?;
        self.rows.iter().position(|row| row.path == path)
    }

    /// A click on a row: folders open and close, files open in the panel.
    pub(super) fn activate_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else { return };
        self.cursor = Some(row.path.clone());
        match row.kind {
            EntryKind::Directory => {
                if !self.search.query.trim().is_empty() {
                    cx.notify();
                    return;
                }
                if let Some(directory) = self.tree.toggle(&row.path) {
                    self.load(directory, false, cx);
                }
                self.mark_dirty();
                cx.notify();
            }
            EntryKind::File => {
                self.tree_selection = Some(row.path.clone());
                self.selected = Some(row.path.clone());
                cx.notify();
                self.state.update(cx, |state, cx| state.open_file(&row.path, None, cx));
            }
        }
    }

    fn move_cursor(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.row_index(self.cursor.as_deref()).or_else(|| self.row_index(self.selected.as_deref()));
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, self.rows.len() as isize - 1) as usize,
            None if step < 0 => self.rows.len() - 1,
            None => 0,
        };
        self.cursor = Some(self.rows[next].path.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(super) fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, cx);
    }

    pub(super) fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, cx);
    }

    /// Right: opens a closed folder, or steps into an open one.
    pub(super) fn expand_selected(&mut self, _: &ExpandSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.row_index(self.cursor.as_deref()) else { return };
        let row = &self.rows[index];
        if row.kind != EntryKind::Directory {
            return;
        }
        if row.expanded {
            self.move_cursor(1, cx);
        } else {
            self.activate_row(index, cx);
        }
    }

    /// Left: closes an open folder, or steps out to the parent.
    pub(super) fn collapse_selected(&mut self, _: &CollapseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.row_index(self.cursor.as_deref()) else { return };
        let row = self.rows[index].clone();
        if row.kind == EntryKind::Directory && row.expanded && self.search.query.trim().is_empty() {
            self.tree.collapse(&row.path);
            self.mark_dirty();
            cx.notify();
            return;
        }
        if let Some(parent) = self.rows[..index].iter().rposition(|candidate| candidate.depth < row.depth) {
            self.cursor = Some(self.rows[parent].path.clone());
            self.scroll.scroll_to_item(parent, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    pub(super) fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.row_index(self.cursor.as_deref()) {
            self.activate_row(index, cx);
        }
    }

    /// Expand all opens every non-ignored folder, listing as it goes; collapse all closes all.
    pub(super) fn toggle_all(&mut self, cx: &mut Context<Self>) {
        if self.tree.expanding_all() || self.tree.all_expanded() {
            self.tree.collapse_all();
        } else {
            for directory in self.tree.expand_all() {
                self.load(directory, false, cx);
            }
        }
        self.mark_dirty();
        cx.notify();
    }
}
