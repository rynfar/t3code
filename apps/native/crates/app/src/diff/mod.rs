//! The diff surface. Ports `components/DiffPanel.tsx`: the scope (Changes, Uncommitted, a turn,
//! one tool call), the requests behind it, collapsed files per scope (`useDiffPanelSessionStore`),
//! and its loading, empty and error states. Patches are fetched and parsed on the network
//! runtime; the view paints only the rows on screen.

mod header;
mod lines;
mod source;
mod tree;
mod view;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    ClipboardItem, Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Pixels, Render,
    Styled, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_component::v_flex;
use t3_client::supervisor::ConnectionStatus;
use t3_core::diff::{
    DiffScope, DiffSelection, Layout, ParsedPatch, ToolCallTarget, TurnDiffSummary, turn_diff_summaries, workspace_mutation_id,
};
use t3_core::right_panel::{Surface, SurfaceKind};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::theme::{Palette, hsla, palette};
use source::{Loaded, RequestKey};
pub use view::Parsed;
use view::View;

/// `LIVE_REFRESH_MIN_INTERVAL_MS`: showing the panel revalidates a git diff older than this.
const GIT_REFRESH_FLOOR: Duration = Duration::from_secs(10);
const CACHE_LIMIT: usize = 6;

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
    /// `deriveWorkspaceMutationId` as last seen: a change refetches git diffs mid-turn.
    workspace_mutation: Option<String>,
    /// Whether each cwd is a repository, learned from git previews (`isGitRepo`).
    repositories: HashMap<String, bool>,
    connected: bool,
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
    /// A patch from outside the thread (a pull request's Code tab) replaces the scopes.
    external: Option<External>,
    _subscription: Subscription,
}

/// What a detached panel shows: a key for its folds, and the patch or why there is none.
pub struct External {
    pub key: String,
    pub parsed: Option<Arc<Parsed>>,
    pub pending: bool,
    pub error: Option<String>,
}

impl DiffPanel {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::construct(state, cx);
        panel.sync(cx);
        panel
    }

    fn construct(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::RightPanel | AppEvent::Shell | AppEvent::Phase) {
                this.sync(cx);
            }
        });
        DiffPanel {
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
            workspace_mutation: None,
            repositories: HashMap::new(),
            connected: false,
            visible: false,
            ui: (Layout::Stacked, false),
            view: None,
            collapsed: HashMap::new(),
            tree_collapsed: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            tree_scroll: UniformListScrollHandle::new(),
            wrap: true,
            ignore_whitespace: crate::settings::client_settings(cx).diff_ignore_whitespace(),
            h_offset: px(0.),
            char_width: None,
            top_file: None,
            selected_file: None,
            handled_reveal: None,
            external: None,
            _subscription: subscription,
        }
    }

    /// A panel that shows `show_external`'s patch instead of the open thread's scopes.
    pub fn detached(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::construct(state, cx);
        panel.external = Some(External { key: String::new(), parsed: None, pending: true, error: None });
        panel
    }

    /// The pull request Code tab uses the same initial fold preference as thread diffs.
    pub fn show_external(&mut self, external: External, cx: &mut Context<Self>) {
        self.external = Some(external);
        self.rebuild(cx);
        cx.notify();
    }

    /// Reconciles with app state. Runs on every thread event, so it does nothing unless the
    /// scope, the request, the connection or the workspace actually changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.external.is_some() {
            let ui = (crate::settings::client_settings(cx).diff_layout(), self.state.read(cx).panels.diff_file_tree_open);
            if ui != self.ui {
                self.ui = ui;
                self.rebuild(cx);
                cx.notify();
            }
            return;
        }
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone();
        if thread_id != self.thread_id {
            self.thread_id = thread_id.clone();
            self.fingerprint = (0, None);
            self.summaries = Arc::new(Vec::new());
            self.workspace_mutation = None;
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
        // A failed request reruns once the connection settles (`runtime.ts` query re-execution).
        let connected = matches!(state.phase, Phase::Online(ConnectionStatus::Connected));
        let reconnected = connected && !self.connected;
        if reconnected {
            self.error = None;
        }
        self.connected = connected;
        let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let fingerprint = projection
            .map(|projection| (projection.checkpoints.len(), projection.checkpoints.last().map(|checkpoint| checkpoint.id.clone())))
            .unwrap_or((0, None));
        let checkpoints_changed = fingerprint != self.fingerprint;
        if checkpoints_changed {
            self.summaries = Arc::new(projection.map(|projection| turn_diff_summaries(&projection.checkpoints)).unwrap_or_default());
            self.fingerprint = fingerprint;
        }
        let mutation = projection.and_then(|projection| {
            let latest_checkpoint = self.summaries.first().map(|summary| summary.completed_at.as_str());
            workspace_mutation_id(projection.visible_turn_items.iter().map(|row| row.item.as_ref()), latest_checkpoint)
        });
        let selection = state.panels.diff_selection(&thread_id);
        if matches!(selection, DiffSelection::Turn { .. }) && checkpoints_changed && !self.summaries.is_empty() {
            let summaries = self.summaries.clone();
            self.state.update(cx, |state, cx| state.reconcile_diff_selection(&summaries, cx));
        }
        let state = self.state.read(cx);
        let environment = state.environment().map(str::to_owned);
        let visible = state.panels.panels.active_surface(&thread_id).is_some_and(|surface| surface.kind == SurfaceKind::Diff);
        let scope = match DiffScope::resolve(&selection, &self.summaries) {
            DiffScope::ToolCall(target) => {
                let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
                DiffScope::ToolCall(target.resolve(projection.map(|projection| projection.visible_turn_items.as_slice()).unwrap_or_default()))
            }
            scope => scope,
        };
        let cwd = state.thread_shell(&thread_id).and_then(|thread| {
            thread.worktree_path.clone().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()))
        });
        let repository = cwd.as_ref().and_then(|cwd| self.repositories.get(cwd)).copied();
        // The agent may have changed files; showing the panel revalidates a stale git diff.
        let git_stale = self.git_fetched_at.is_some_and(|at| at.elapsed() >= GIT_REFRESH_FLOOR);
        if (mutation != self.workspace_mutation && self.git_fetched_at.is_some()) || (visible && !self.visible && git_stale) {
            self.git_revision += 1;
        }
        self.workspace_mutation = mutation;
        self.visible = visible;
        let request = environment.and_then(|environment| match &scope {
            DiffScope::Branch { base_ref } => cwd.clone().map(|cwd| RequestKey::Git {
                environment,
                cwd,
                base_ref: base_ref.clone(),
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            DiffScope::Unstaged => cwd.clone().map(|cwd| RequestKey::Git {
                environment,
                cwd,
                base_ref: None,
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            // `useCheckpointDiff` is disabled outside a repository.
            DiffScope::Turn { summary_index, .. } if repository != Some(false) => Some(RequestKey::Turn {
                environment,
                thread_id: thread_id.clone(),
                turn_count: self.summaries[*summary_index].turn_count,
                ignore_whitespace: self.ignore_whitespace,
            }),
            DiffScope::Turn { .. } | DiffScope::NoTurns => None,
            DiffScope::ToolCall(target) => Some(RequestKey::ToolCall { environment, target: target.clone() }),
        });
        let ui = (crate::settings::client_settings(cx).diff_layout(), state.panels.diff_file_tree_open);
        let changed = request != self.request
            || scope != self.scope
            || selection != self.selection
            || checkpoints_changed
            || reconnected
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
                    None if visible && connected => self.fetch(key, cx),
                    None => self.pending = false,
                }
            }
        } else if visible
            && connected
            && !self.pending
            && self.error.is_none()
            && let Some(key) = self.request.clone()
            && source::needs_fetch(&key, self.loaded.as_ref().map(|(loaded, _)| loaded))
        {
            self.fetch(key, cx);
        }
        if changed {
            self.rebuild(cx);
            cx.notify();
        }
    }

    /// Whether the current request's source has something to show, even nothing (a cwd that is
    /// not a repository), possibly from an older revision while the current one is fetched.
    fn has_loaded(&self) -> bool {
        match (&self.request, &self.loaded) {
            (Some(request), Some((key, _))) => key.same_source(request),
            _ => false,
        }
    }

    fn fetch(&mut self, key: RequestKey, cx: &mut Context<Self>) {
        let Some(task) = source::fetch(self.state.read(cx), &key) else {
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
                        if let (RequestKey::Git { cwd, .. }, Loaded::Git { repository, .. }) = (&key, &loaded) {
                            this.git_fetched_at = Some(Instant::now());
                            this.repositories.insert(cwd.clone(), *repository);
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
        if let Some(external) = &self.external {
            return external.parsed.clone();
        }
        if !self.has_loaded() {
            return None;
        }
        match (&self.loaded.as_ref()?.1, &self.scope) {
            (Loaded::Git { branch, .. }, DiffScope::Branch { .. }) => branch.clone(),
            (Loaded::Git { unstaged, .. }, DiffScope::Unstaged) => unstaged.clone(),
            (Loaded::Turn(parsed), DiffScope::Turn { .. }) => Some(parsed.clone()),
            (Loaded::ToolCall(call), DiffScope::ToolCall(_)) => call.parsed(self.ignore_whitespace),
            _ => None,
        }
    }

    /// The loaded tool call for the scope on screen.
    fn tool_call(&self) -> Option<Arc<source::ToolCall>> {
        match &self.loaded.as_ref().filter(|_| self.has_loaded())?.1 {
            Loaded::ToolCall(call) => Some(call.clone()),
            _ => None,
        }
    }

    /// `reviewSectionId`: folds are remembered per thread and scope.
    fn scope_key(&self) -> String {
        if let Some(external) = &self.external {
            return external.key.clone();
        }
        let section = match &self.scope {
            DiffScope::Branch { .. } => "branch".to_owned(),
            DiffScope::Unstaged => "unstaged".to_owned(),
            DiffScope::Turn { summary_index, .. } => format!("turn:{}", self.summaries[*summary_index].run_id),
            DiffScope::NoTurns => "none".to_owned(),
            DiffScope::ToolCall(target) => format!("tool-call:{}:{}", target.thread_id, target.item_id),
        };
        format!("{}:{section}", self.thread_id.as_deref().unwrap_or_default())
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = self.current() else {
            if self.view.take().is_some() {
                self.list.reset(0);
            }
            return;
        };
        let scope_key = self.scope_key();
        let layout = crate::settings::client_settings(cx).diff_layout();
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
        let folds = self.collapsed.entry(scope_key.clone()).or_insert_with(|| {
            if crate::settings::client_settings(cx).diff_files_collapsed() {
                parsed
                    .files()
                    .iter()
                    .map(t3_core::diff::FileDiff::key)
                    .collect()
            } else {
                HashSet::new()
            }
        });
        let view = View::new(parsed, scope_key.clone(), layout, Some(folds), &self.tree_collapsed);
        let count = view.row_count();
        self.list.reset_with_uniform_height(count, px(lines::LINE_HEIGHT));
        match keep.filter(|top| top.item_ix < count) {
            Some(top) => self.list.scroll_to(top),
            None => self.h_offset = px(0.),
        }
        self.top_file = view.row(0).map(|row| row.file());
        self.view = Some(view);
        self.reveal_requested(cx);
    }

    /// Folds or unfolds one file, splicing only its rows so the list keeps its place.
    fn set_folded(&mut self, file: usize, folded: bool, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_mut() else { return };
        let Some((range, count)) = view.set_folded(file, folded) else { return };
        let key = view.key(file).map(str::to_owned);
        let defaults = || view.folded_keys();
        let folds = self.collapsed.entry(view.scope_key.clone()).or_insert_with(defaults);
        if let Some(key) = key {
            if folded {
                folds.insert(key);
            } else {
                folds.remove(&key);
            }
        }
        self.list.splice(range, count);
        cx.notify();
    }

    pub fn toggle_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let folded = self.is_collapsed(file);
        self.set_folded(file, !folded, cx);
    }

    /// `toggleAllDiffFiles`: expands every file when all are collapsed, else collapses all.
    pub fn toggle_all_files(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_mut() else { return };
        let fold = !view.all_folded();
        view.set_all_folded(fold);
        self.collapsed.insert(view.scope_key.clone(), view.folded_keys());
        self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
        cx.notify();
    }

    /// Runs `handler` after each scroll of the rows, for a detached panel that loads more as
    /// the reader nears the end.
    pub fn on_rows_scrolled(&self, handler: impl FnMut(&gpui::ListScrollEvent, &mut Window, &mut gpui::App) + 'static) {
        self.list.set_scroll_handler(handler);
    }

    /// Whether the last row sits within `margin` below the viewport, as last laid out.
    pub fn near_end(&self, margin: Pixels) -> bool {
        let count = self.list.item_count();
        count > 0 && self.list.bounds_for_item(count - 1).is_some_and(|row| row.top() <= self.list.viewport_bounds().bottom() + margin)
    }

    pub fn is_external(&self) -> bool {
        self.external.is_some()
    }

    pub fn all_collapsed(&self) -> bool {
        self.view.as_ref().is_some_and(View::all_folded)
    }

    pub fn is_collapsed(&self, file: usize) -> bool {
        self.view.as_ref().is_some_and(|view| view.is_folded(file))
    }

    /// Reveals a file: unfolds it and scrolls its header to the top (`revealDiffFile`).
    pub fn reveal_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|file| file.path().to_owned()) else {
            return;
        };
        self.set_folded(file, false, cx);
        if let Some(index) = self.view.as_ref().and_then(|view| view.header_row(file)) {
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
        self.error = None;
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
        // A tool call hides whitespace itself, so its request stays the same.
        self.rebuild(cx);
        cx.notify();
    }

    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        crate::settings::store::update_client(&self.state, t3_core::client_settings::ClientSettingChange::DiffLayout(layout), cx);
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
            view.rebuild_tree(&self.tree_collapsed);
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
        let message = |text: &str| centered_message(text, p);
        if let Some(external) = &self.external
            && self.view.is_none()
        {
            return match (&external.error, external.pending) {
                (Some(error), _) => message(error),
                (None, true) => lines::loading_state(cx).into_any_element(),
                (None, false) => message("No patch available for this selection."),
            };
        }
        if self.thread_id.is_none() && self.external.is_none() {
            return message("Select a thread to inspect turn diffs.");
        }
        if matches!(self.scope, DiffScope::ToolCall(_)) {
            return self.render_tool_call(window, cx);
        }
        if self.is_outside_repository(cx) {
            return message("Turn diffs are unavailable because this project is not a git repository.");
        }
        if self.scope == DiffScope::NoTurns {
            return message("No completed turns yet.");
        }
        let Some(parsed) = self.view.as_ref().map(|view| view.parsed.clone()) else {
            if self.pending || (self.request.is_some() && self.error.is_none() && !self.has_loaded()) {
                return lines::loading_state(cx).into_any_element();
            }
            return v_flex()
                .flex_1()
                .children(self.error.clone().map(|error| error_line(error, p)))
                .child(message("No patch available for this selection."))
                .into_any_element();
        };
        match &parsed.patch {
            ParsedPatch::Empty => message("No net changes in this selection."),
            ParsedPatch::Raw { text, reason } => lines::raw_patch(text, reason, self.wrap, cx).into_any_element(),
            ParsedPatch::Files(_) => self.render_files(&parsed, window, cx),
        }
    }

    /// `!isGitRepo`: the thread's cwd answered a git preview with no sources.
    fn is_outside_repository(&self, cx: &gpui::App) -> bool {
        let Some(thread_id) = self.thread_id.as_deref() else { return false };
        let state = self.state.read(cx);
        let cwd = state.thread_shell(thread_id).and_then(|thread| {
            thread.worktree_path.as_deref().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.as_str()))
        });
        cwd.and_then(|cwd| self.repositories.get(cwd)) == Some(&false)
    }

    fn render_files(&mut self, parsed: &Parsed, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let truncated = parsed.truncated.then(|| {
            let suffix = if parsed.totals.is_some() { " Totals include all changes." } else { "" };
            notice(format!("This preview exceeds the size limit. Changes shown are incomplete.{suffix}"), p)
        });
        v_flex().flex_1().min_h_0().children(truncated).child(self.files_body(window, cx)).into_any_element()
    }

    fn files_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let tree_open = self.state.read(cx).panels.diff_file_tree_open;
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(self.render_list(window, cx))
            .when(tree_open, |this| this.child(tree::render(self, cx)))
    }

    /// The tool-call scope (`toolCallSelection` in `DiffPanel.tsx`). It reads no repository, so
    /// it works outside one; the recorded content shows when the provider left no diff.
    fn render_tool_call(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let call = self.tool_call();
        // `getRenderablePatch` treats a blank patch as none.
        let parsed = self.view.as_ref().map(|view| view.parsed.clone()).filter(|parsed| parsed.patch != ParsedPatch::Empty);
        let truncated = call.as_ref().is_some_and(|call| call.diff.truncated).then(|| notice("This preview exceeds the size limit. Changes shown are incomplete.", p));
        let omitted = match (&call, parsed.as_deref().map(|parsed| &parsed.patch)) {
            (Some(call), Some(ParsedPatch::Files(files))) => Some((call.diff.recorded_files, call.diff.omitted_files(files.len()))),
            _ => None,
        };
        let omitted = omitted.filter(|(_, omitted)| *omitted > 0).map(|(recorded, omitted)| {
            notice(format!("This tool call changed {recorded} files. The recorded patch omits {omitted} of them."), p)
        });
        let error = self.error.clone().filter(|_| parsed.is_none()).map(|error| error_line(error, p));
        let body = match parsed.as_deref().map(|parsed| &parsed.patch) {
            Some(ParsedPatch::Files(_)) => self.files_body(window, cx).into_any_element(),
            Some(ParsedPatch::Raw { text, reason }) => lines::raw_patch(text, reason, self.wrap, cx).into_any_element(),
            Some(ParsedPatch::Empty) | None => match call.as_ref().filter(|call| !call.diff.content.is_empty()) {
                Some(call) => lines::recorded_content(&call.diff.content, self.wrap, cx).into_any_element(),
                None if self.pending || (self.error.is_none() && !self.has_loaded()) => lines::loading_state(cx).into_any_element(),
                // A blank patch parses to `Empty`; a missing one leaves no view.
                None if self.view.is_some() => centered_message("No net changes in this selection.", p),
                None => centered_message("No patch available for this selection.", p),
            },
        };
        v_flex().flex_1().min_h_0().children(truncated).children(omitted).children(error).child(body).into_any_element()
    }
}

fn centered_message(text: &str, p: &Palette) -> gpui::AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(20.))
        .text_center()
        .text_xs()
        .text_color(hsla(p.muted_foreground).opacity(0.7))
        .child(text.to_owned())
        .into_any_element()
}

/// The strip above the files that says what the patch leaves out.
fn notice(text: impl Into<gpui::SharedString>, p: &Palette) -> gpui::Div {
    div()
        .flex_none()
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.7))
        .bg(hsla(p.muted).opacity(0.4))
        .px(px(12.))
        .py(px(6.))
        .text_size(px(11.))
        .text_color(hsla(p.muted_foreground))
        .child(text.into())
}

fn error_line(error: String, p: &Palette) -> gpui::Div {
    div().px(px(12.)).mb(px(8.)).text_size(px(11.)).text_color(hsla(p.error).opacity(0.8)).child(error)
}

impl AppState {
    /// `onOpenToolDiff`: a file change's "Open diff" shows that tool call's patch in the diff.
    pub fn open_tool_call_diff(&mut self, target: ToolCallTarget, cx: &mut Context<Self>) {
        // Opening resets a diff nobody asked for to the branch scope, so select after it.
        self.mutate_panels(cx, |state, thread| {
            state.panels.open(thread, Surface::diff());
            true
        });
        self.select_diff(DiffSelection::ToolCall(target), cx);
    }
}

impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(header::render(self, window, cx))
            .child(self.render_body(window, cx))
    }
}
