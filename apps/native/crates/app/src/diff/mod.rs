//! The diff surface. Ports `components/DiffPanel.tsx`: the scope (Changes, Uncommitted, a turn),
//! the requests behind it, collapsed files per scope (`useDiffPanelSessionStore`), and its
//! loading, empty and error states. Patches are fetched and parsed on the network runtime; the
//! view paints only the rows on screen.

mod header;
mod lines;
mod tree;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    ClipboardItem, Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Pixels, Render,
    Styled, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_component::v_flex;
use t3_core::diff::{
    DiffRow, DiffScope, DiffSelection, FileDiff, Layout, ParsedPatch, PreviewSourceKind, TreeRow, TurnDiffSummary,
    build_rows, checkpoint_diff_request, decode_diff_preview, decode_turn_diff, diff_preview_request, file_tree_rows,
    parse_patch, turn_diff_summaries,
};
use t3_core::right_panel::SurfaceKind;

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};

/// `LIVE_REFRESH_MIN_INTERVAL_MS`: showing the panel revalidates a git diff older than this.
const GIT_REFRESH_FLOOR: Duration = Duration::from_secs(10);
const CACHE_LIMIT: usize = 6;

/// One fetched patch, parsed off the UI thread.
pub struct Parsed {
    pub patch: ParsedPatch,
    pub truncated: bool,
    /// Totals over every changed file when the patch holds only some of them.
    pub totals: Option<(u64, u64)>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    /// The longest line in bytes, for horizontal scrolling without wrap.
    pub max_line_len: usize,
}

impl Parsed {
    fn new(patch: &str, truncated: bool, totals: Option<(u64, u64)>, base_ref: Option<String>, head_ref: Option<String>) -> Arc<Parsed> {
        let patch = parse_patch(patch);
        let max_line_len = match &patch {
            ParsedPatch::Files(files) => {
                files.iter().flat_map(|file| &file.hunks).flat_map(|hunk| &hunk.lines).map(|line| line.text.len()).max().unwrap_or(0)
            }
            _ => 0,
        };
        Arc::new(Parsed { patch, truncated, totals, base_ref, head_ref, max_line_len })
    }

    pub fn files(&self) -> &[FileDiff] {
        match &self.patch {
            ParsedPatch::Files(files) => files,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestKey {
    /// `review.getDiffPreview` answers both git scopes. `revision` bumps on refresh.
    Git { cwd: String, base_ref: Option<String>, ignore_whitespace: bool, revision: u64 },
    Turn { thread_id: String, turn_count: u64, ignore_whitespace: bool },
}

impl RequestKey {
    /// A refresh of the same scope keeps showing the old patch until the new one lands.
    fn same_source(&self, other: &RequestKey) -> bool {
        match (self, other) {
            (RequestKey::Git { cwd, base_ref, ignore_whitespace, .. }, RequestKey::Git { cwd: c, base_ref: b, ignore_whitespace: w, .. }) => {
                cwd == c && base_ref == b && ignore_whitespace == w
            }
            _ => self == other,
        }
    }
}

#[derive(Clone)]
enum Loaded {
    Git { branch: Option<Arc<Parsed>>, unstaged: Option<Arc<Parsed>> },
    Turn(Arc<Parsed>),
}

/// What the list paints: rebuilt when the patch, scope, layout or folds change.
pub struct View {
    pub parsed: Arc<Parsed>,
    pub scope_key: String,
    pub layout: Layout,
    pub rows: Vec<DiffRow>,
    pub tree: Vec<TreeRow>,
    /// Line-number digits per file: each file sizes its own gutter, as Pierre does.
    pub digits: Vec<usize>,
}

pub struct DiffPanel {
    state: Entity<AppState>,
    thread_id: Option<String>,
    fingerprint: (usize, Option<String>),
    summaries: Arc<Vec<TurnDiffSummary>>,
    selection: DiffSelection,
    scope: DiffScope,
    request: Option<RequestKey>,
    loaded: Option<(RequestKey, Loaded)>,
    cache: Vec<(RequestKey, Loaded)>,
    pending: bool,
    error: Option<String>,
    _fetch: Option<Task<()>>,
    git_revision: u64,
    git_fetched_at: Option<Instant>,
    visible: bool,
    /// The persisted layout and file tree toggle, as last rendered.
    ui: (Layout, bool),
    view: Option<View>,
    /// Session-only folds per scope; a scope with no entry starts all collapsed
    /// (`diffFilesCollapsed` defaults to true).
    collapsed: HashMap<String, HashSet<String>>,
    tree_collapsed: HashSet<String>,
    list: ListState,
    tree_scroll: UniformListScrollHandle,
    wrap: bool,
    ignore_whitespace: bool,
    h_offset: Pixels,
    char_width: Option<Pixels>,
    top_file: Option<usize>,
    selected_file: Option<String>,
    handled_reveal: Option<(String, String, u64)>,
    _subscription: Subscription,
}

impl DiffPanel {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::RightPanel | AppEvent::Shell | AppEvent::Phase) {
                this.sync(cx);
            }
        });
        let list = ListState::new(0, ListAlignment::Top, px(400.));
        let mut panel = DiffPanel {
            state,
            thread_id: None,
            fingerprint: (0, None),
            summaries: Arc::new(Vec::new()),
            selection: DiffSelection::default(),
            scope: DiffScope::Unstaged,
            request: None,
            loaded: None,
            cache: Vec::new(),
            pending: false,
            error: None,
            _fetch: None,
            git_revision: 0,
            git_fetched_at: None,
            visible: false,
            ui: (Layout::Stacked, false),
            view: None,
            collapsed: HashMap::new(),
            tree_collapsed: HashSet::new(),
            list,
            tree_scroll: UniformListScrollHandle::new(),
            wrap: true,
            ignore_whitespace: true,
            h_offset: px(0.),
            char_width: None,
            top_file: None,
            selected_file: None,
            handled_reveal: None,
            _subscription: subscription,
        };
        panel.sync(cx);
        panel
    }

    /// Reconciles with app state. Runs on every thread event, so it does nothing unless the
    /// scope, the request or the turns actually changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone();
        if thread_id != self.thread_id {
            self.thread_id = thread_id.clone();
            self.fingerprint = (0, None);
            self.summaries = Arc::new(Vec::new());
            self.selected_file = None;
            self.handled_reveal = None;
            self.tree_collapsed.clear();
        }
        let Some(thread_id) = thread_id else {
            if self.request.take().is_some() | self.view.take().is_some() {
                self.list.reset(0);
                cx.notify();
            }
            return;
        };
        let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let fingerprint = projection
            .map(|projection| (projection.checkpoints.len(), projection.checkpoints.last().map(|checkpoint| checkpoint.id.clone())))
            .unwrap_or((0, None));
        let checkpoints_changed = fingerprint != self.fingerprint;
        if checkpoints_changed {
            self.summaries = Arc::new(projection.map(|projection| turn_diff_summaries(&projection.checkpoints)).unwrap_or_default());
            self.fingerprint = fingerprint;
        }
        let selection = state.panels.diff_selection(&thread_id);
        if matches!(selection, DiffSelection::Turn { .. }) && checkpoints_changed && !self.summaries.is_empty() {
            let summaries = self.summaries.clone();
            self.state.update(cx, |state, cx| state.reconcile_diff_selection(&summaries, cx));
        }
        let state = self.state.read(cx);
        let visible = state.panels.panels.active_surface(&thread_id).is_some_and(|surface| surface.kind == SurfaceKind::Diff);
        let scope = DiffScope::resolve(&selection, &self.summaries);
        let cwd = state.shell.thread(&thread_id).and_then(|thread| {
            thread.worktree_path.clone().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()))
        });
        // A finished turn changed the workspace; showing the panel revalidates a stale git diff.
        let git_stale = self.git_fetched_at.is_some_and(|at| at.elapsed() >= GIT_REFRESH_FLOOR);
        if (checkpoints_changed && self.git_fetched_at.is_some()) || (visible && !self.visible && git_stale) {
            self.git_revision += 1;
        }
        self.visible = visible;
        let request = match &scope {
            DiffScope::Branch { base_ref } => cwd.map(|cwd| RequestKey::Git {
                cwd,
                base_ref: base_ref.clone(),
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            DiffScope::Unstaged => cwd.map(|cwd| RequestKey::Git {
                cwd,
                base_ref: None,
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            DiffScope::Turn { summary_index, .. } => Some(RequestKey::Turn {
                thread_id: thread_id.clone(),
                turn_count: self.summaries[*summary_index].turn_count,
                ignore_whitespace: self.ignore_whitespace,
            }),
            DiffScope::NoTurns => None,
        };
        let ui = (state.panels.diff_layout, state.panels.diff_file_tree_open);
        let changed = request != self.request
            || scope != self.scope
            || selection != self.selection
            || checkpoints_changed
            || ui != self.ui;
        self.ui = ui;
        self.selection = selection;
        self.scope = scope;
        if request != self.request {
            self.request = request.clone();
            self.error = None;
            if let Some(key) = request {
                match self.cache.iter().position(|(cached, _)| *cached == key) {
                    Some(index) => {
                        let entry = self.cache.remove(index);
                        self.cache.push(entry.clone());
                        self.loaded = Some(entry);
                        self.pending = false;
                    }
                    None if visible => self.fetch(key, cx),
                    None => self.pending = false,
                }
            }
        } else if visible
            && !self.pending
            && self.error.is_none()
            && self.current().is_none()
            && let Some(key) = self.request.clone()
        {
            self.fetch(key, cx);
        }
        if changed {
            self.rebuild(cx);
            cx.notify();
        }
    }

    fn fetch(&mut self, key: RequestKey, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let task = match &key {
            RequestKey::Git { cwd, base_ref, ignore_whitespace, .. } => {
                let (method, payload) = diff_preview_request(cwd, base_ref.as_deref(), *ignore_whitespace);
                state.request(method, payload, |value| {
                    let source = |kind| {
                        decode_diff_preview(&value, kind)
                            .map(|source| Parsed::new(&source.diff, source.truncated, source.totals, source.base_ref, source.head_ref))
                    };
                    Ok(Loaded::Git { branch: source(PreviewSourceKind::BranchRange), unstaged: source(PreviewSourceKind::WorkingTree) })
                })
            }
            RequestKey::Turn { thread_id, turn_count, ignore_whitespace } => {
                let (method, payload) = checkpoint_diff_request(thread_id, *turn_count, *ignore_whitespace);
                state.request(method, payload, |value| {
                    decode_turn_diff(&value).map(|patch| Loaded::Turn(Parsed::new(&patch, false, None, None, None)))
                })
            }
        };
        let Some(task) = task else {
            self.error = Some("Not connected to the server.".to_owned());
            return;
        };
        self.pending = true;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.request.as_ref() != Some(&key) {
                    return;
                }
                this.pending = false;
                match result {
                    Ok(loaded) => {
                        if matches!(key, RequestKey::Git { .. }) {
                            this.git_fetched_at = Some(Instant::now());
                        }
                        this.cache.retain(|(cached, _)| !cached.same_source(&key));
                        this.cache.push((key.clone(), loaded.clone()));
                        if this.cache.len() > CACHE_LIMIT {
                            this.cache.remove(0);
                        }
                        this.loaded = Some((key, loaded));
                    }
                    Err(error) => this.error = Some(error),
                }
                this.rebuild(cx);
                cx.notify();
            });
        }));
    }

    /// The patch for the current scope: the loaded one, or the last one of the same source
    /// while a refresh is in flight.
    fn current(&self) -> Option<Arc<Parsed>> {
        let request = self.request.as_ref()?;
        let (key, loaded) = self.loaded.as_ref()?;
        if !key.same_source(request) {
            return None;
        }
        match (loaded, &self.scope) {
            (Loaded::Git { branch, .. }, DiffScope::Branch { .. }) => branch.clone(),
            (Loaded::Git { unstaged, .. }, DiffScope::Unstaged) => unstaged.clone(),
            (Loaded::Turn(parsed), DiffScope::Turn { .. }) => Some(parsed.clone()),
            _ => None,
        }
    }

    /// `reviewSectionId`: folds are remembered per thread and scope.
    fn scope_key(&self) -> String {
        let section = match &self.scope {
            DiffScope::Branch { .. } => "branch".to_owned(),
            DiffScope::Unstaged => "unstaged".to_owned(),
            DiffScope::Turn { summary_index, .. } => format!("turn:{}", self.summaries[*summary_index].run_id),
            DiffScope::NoTurns => "none".to_owned(),
        };
        format!("{}:{section}", self.thread_id.as_deref().unwrap_or_default())
    }

    fn collapsed_for(&self, scope_key: &str, files: &[FileDiff]) -> HashSet<String> {
        self.collapsed.get(scope_key).cloned().unwrap_or_else(|| files.iter().map(FileDiff::key).collect())
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = self.current() else {
            if self.view.take().is_some() {
                self.list.reset(0);
            }
            return;
        };
        let scope_key = self.scope_key();
        let layout = self.state.read(cx).panels.diff_layout;
        let same = self.view.as_ref().is_some_and(|view| {
            Arc::ptr_eq(&view.parsed, &parsed) && view.scope_key == scope_key && view.layout == layout
        });
        if same {
            self.reveal_requested(cx);
            return;
        }
        // A refresh of the scope on screen keeps the reader's place.
        let keep = self
            .view
            .as_ref()
            .filter(|view| view.scope_key == scope_key && view.layout == layout)
            .map(|_| self.list.logical_scroll_top());
        let files = parsed.files();
        let collapsed = self.collapsed_for(&scope_key, files);
        let rows = build_rows(files, &collapsed, layout);
        let tree = file_tree_rows(files, &self.tree_collapsed);
        let digits = files.iter().map(gutter_digits).collect();
        self.list.reset_with_uniform_height(rows.len(), px(lines::LINE_HEIGHT));
        match keep.filter(|top| top.item_ix < rows.len()) {
            Some(top) => self.list.scroll_to(top),
            None => self.h_offset = px(0.),
        }
        self.top_file = rows.first().map(DiffRow::file);
        self.view = Some(View { parsed, scope_key, layout, rows, tree, digits });
        self.reveal_requested(cx);
    }

    /// Re-derives rows after folds change, splicing only the file that changed so the list
    /// keeps its scroll position and measurements.
    fn refold(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_mut() else { return };
        let files = view.parsed.files();
        let collapsed = self.collapsed.get(&view.scope_key).cloned().unwrap_or_default();
        let rows = build_rows(files, &collapsed, view.layout);
        match file {
            Some(file) => {
                let range = |rows: &[DiffRow]| {
                    let start = rows.iter().position(|row| row.file() == file).unwrap_or(rows.len());
                    let end = rows[start..].iter().position(|row| row.file() != file).map_or(rows.len(), |offset| start + offset);
                    start..end
                };
                let old = range(&view.rows);
                let new = range(&rows);
                self.list.splice(old, new.len());
            }
            None => self.list.reset_with_uniform_height(rows.len(), px(lines::LINE_HEIGHT)),
        }
        view.rows = rows;
        cx.notify();
    }

    fn fold_set(&mut self) -> Option<&mut HashSet<String>> {
        let view = self.view.as_ref()?;
        let defaults: HashSet<String> = view.parsed.files().iter().map(FileDiff::key).collect();
        Some(self.collapsed.entry(view.scope_key.clone()).or_insert(defaults))
    }

    pub fn toggle_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(key) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(FileDiff::key) else { return };
        let Some(folds) = self.fold_set() else { return };
        if !folds.remove(&key) {
            folds.insert(key);
        }
        self.refold(Some(file), cx);
    }

    /// `toggleAllDiffFiles`: expands every file when all are collapsed, else collapses all.
    pub fn toggle_all_files(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_ref() else { return };
        let keys: HashSet<String> = view.parsed.files().iter().map(FileDiff::key).collect();
        let all_collapsed = self.all_collapsed();
        let scope_key = view.scope_key.clone();
        self.collapsed.insert(scope_key, if all_collapsed { HashSet::new() } else { keys });
        self.refold(None, cx);
    }

    pub fn all_collapsed(&self) -> bool {
        let Some(view) = self.view.as_ref() else { return false };
        let files = view.parsed.files();
        let folds = self.collapsed.get(&view.scope_key);
        !files.is_empty() && files.iter().all(|file| folds.is_none_or(|folds| folds.contains(&file.key())))
    }

    pub fn is_collapsed(&self, file: usize) -> bool {
        let Some(view) = self.view.as_ref() else { return false };
        let Some(file) = view.parsed.files().get(file) else { return false };
        self.collapsed.get(&view.scope_key).is_none_or(|folds| folds.contains(&file.key()))
    }

    /// Reveals a file: unfolds it and scrolls its header to the top (`revealDiffFile`).
    pub fn reveal_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|file| file.path().to_owned()) else {
            return;
        };
        if self.is_collapsed(file) {
            self.toggle_file(file, cx);
        }
        if let Some(index) = self.view.as_ref().and_then(|view| view.rows.iter().position(|row| *row == DiffRow::FileHeader { file })) {
            self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.) });
            self.top_file = Some(file);
        }
        self.selected_file = Some(path);
        cx.notify();
    }

    /// A turn's "files changed" link asked for a file: reveal it once per request.
    fn reveal_requested(&mut self, cx: &mut Context<Self>) {
        let DiffSelection::Turn { file_path: Some(path), reveal_request_id, .. } = &self.selection else { return };
        let Some(view) = self.view.as_ref() else { return };
        let marker = (view.scope_key.clone(), path.clone(), *reveal_request_id);
        if self.handled_reveal.as_ref() == Some(&marker) {
            return;
        }
        if let Some(file) = view.parsed.files().iter().position(|file| file.path() == path) {
            self.handled_reveal = Some(marker);
            self.reveal_file(file, cx);
        }
    }

    pub fn select(&mut self, selection: DiffSelection, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.select_diff(selection, cx));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.git_revision += 1;
        self.sync(cx);
    }

    pub fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        self.h_offset = px(0.);
        self.list.remeasure();
        cx.notify();
    }

    pub fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.ignore_whitespace = !self.ignore_whitespace;
        self.sync(cx);
    }

    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.set_diff_layout(layout, cx));
    }

    pub fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let open = state.panels.diff_file_tree_open;
            state.set_diff_file_tree_open(!open, cx);
        });
    }

    pub fn toggle_directory(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.tree_collapsed.remove(path) {
            self.tree_collapsed.insert(path.to_owned());
        }
        if let Some(view) = self.view.as_mut() {
            view.tree = file_tree_rows(view.parsed.files(), &self.tree_collapsed);
        }
        cx.notify();
    }

    pub fn copy_path(&mut self, file: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|file| file.path().to_owned()) {
            cx.write_to_clipboard(ClipboardItem::new_string(path));
        }
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let message = |text: String| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(20.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground).opacity(0.7))
                .child(text)
                .into_any_element()
        };
        if self.thread_id.is_none() {
            return message("Select a thread to inspect turn diffs.".to_owned());
        }
        if self.scope == DiffScope::NoTurns {
            return message("No completed turns yet.".to_owned());
        }
        let Some(parsed) = self.view.as_ref().map(|view| view.parsed.clone()) else {
            if self.pending || (self.request.is_some() && self.error.is_none()) {
                return lines::loading_state(cx).into_any_element();
            }
            return v_flex()
                .flex_1()
                .children(self.error.clone().map(|error| {
                    div().px(px(12.)).mb(px(8.)).text_size(px(11.)).text_color(hsla(p.error).opacity(0.8)).child(error)
                }))
                .child(message("No patch available for this selection.".to_owned()))
                .into_any_element();
        };
        match &parsed.patch {
            ParsedPatch::Empty => message("No net changes in this selection.".to_owned()),
            ParsedPatch::Raw { text, reason } => lines::raw_patch(text, reason, self.wrap, cx).into_any_element(),
            ParsedPatch::Files(_) => self.render_files(&parsed, window, cx),
        }
    }

    fn render_files(&mut self, parsed: &Parsed, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let tree_open = self.state.read(cx).panels.diff_file_tree_open;
        let truncated = parsed.truncated.then(|| {
            let suffix = if parsed.totals.is_some() { " Totals include all changes." } else { "" };
            div()
                .flex_none()
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.7))
                .bg(hsla(p.muted).opacity(0.4))
                .px(px(12.))
                .py(px(6.))
                .text_size(px(11.))
                .text_color(hsla(p.muted_foreground))
                .child(format!("This preview exceeds the size limit. Changes shown are incomplete.{suffix}"))
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .children(truncated)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(window, cx))
                    .when(tree_open, |this| this.child(tree::render(self, cx))),
            )
            .into_any_element()
    }
}

/// Digits in the largest line number a file shows.
fn gutter_digits(file: &FileDiff) -> usize {
    let largest = file
        .hunks
        .iter()
        .map(|hunk| (hunk.old_start + hunk.old_count).max(hunk.new_start + hunk.new_count))
        .max()
        .unwrap_or(1);
    largest.max(1).ilog10() as usize + 1
}

impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(header::render(self, cx))
            .child(self.render_body(window, cx))
    }
}
