//! A terminal pane: one session painted by `TerminalElement`, with keyboard and IME input,
//! selection, scrollback, the clipboard, and mouse reports for programs that track the mouse.
//! Ports `TerminalSessionViewport` in `apps/web/src/components/ThreadTerminalDrawer.tsx` (its
//! context menu, the selection action menu, "Add to chat", editing shortcuts and exit handling),
//! `observeSelectionActions` from `apps/web/src/lib/selectionActions.ts`, and the pointer
//! handling of `apps/web/src/terminal/ghostty/surface.ts`.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, UTF16Selection, Window, div, px,
};
use gpui_component::menu::ContextMenuExt;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::client_settings::BrowserLinkTarget;
use t3_core::composer::context::{normalize_text, selection_line_range};
use t3_core::files::metadata::{self, GET_METADATA, Lookup};
use t3_core::markdown::links::open_in_editor_request_with_root;
use t3_core::project_search::{OPEN_IN_EDITOR, preferred_editor};
use t3_core::terminal::emulator::{self, GridPoint, MouseAction, SelectionType, Side, TermMode, ThemeColors, hit_cell};
use t3_core::terminal::links::{self, LinkHit};
use t3_core::terminal::{
    self, Grid, KeyInput, TerminalShortcut, encode_key, encode_paste, local_shortcut, shortcut_data,
};

use super::colors;
use super::drawer::SharedMetadataCache;
use super::element::{RowCache, TerminalElement, TerminalFont};
use super::session::{Frame, Session, Signal, Target};
use super::{AddToChat, Clear, Copy, JumpToLatest, Paste, SelectAll};
use crate::app_state::AppState;
use crate::composer::controls::{MenuItem, menu_surface};

/// The PTY hears a new grid only after the size settles, so a drag does not reflow the prompt.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
const DRAG_THRESHOLD: f32 = 2.;
const LINK_DRAG_THRESHOLD: f32 = 4.;
/// `TERMINAL_LINK_HOVER_DELAY_MS`: wait until a row stops changing before asking for metadata.
const LINK_HOVER_DELAY: Duration = Duration::from_millis(250);
/// `SELECTION_MULTI_CLICK_INTERVAL_MS`: after a double or triple click the selection menu waits
/// out the next click.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const FILESYSTEM_READ_SCOPE: &str = "filesystem:read";
const PREVIEW_OPERATE_SCOPE: &str = "preview:operate";
#[cfg(target_os = "macos")]
const MAC: bool = true;
#[cfg(not(target_os = "macos"))]
const MAC: bool = false;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell_width: f32,
    pub line_height: f32,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// The shell ended while attached.
    Exited,
    /// The user clicked into this pane.
    Activated,
    /// "Add to chat" on a selection; the drawer names the terminal.
    AddToChat(Excerpt),
}

/// A selection's text and 1-based lines, counted from the top of the scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

/// The selection action menu, open at the pointer that ended a selection.
struct SelectionMenu {
    at: Point<Pixels>,
}

#[derive(Clone, Copy)]
struct Drag {
    origin: Point<Pixels>,
    armed: bool,
}

enum LinkValidation {
    Ready(bool),
    Pending(Task<bool>),
}

struct LinkActivation {
    origin: Point<Pixels>,
    link: LinkHit,
    validation: LinkValidation,
}

#[derive(Clone, Copy)]
struct RowStability {
    hash: u64,
    changed_at: Instant,
}

/// `shouldReportTerminalMouse`: Shift, Control and Command keep the pointer for selection.
fn reports_mouse(modifiers: &Modifiers) -> bool {
    !modifiers.shift && !modifiers.control && !modifiers.platform
}

/// `ghosttyMouseButton` for the buttons a pointer press can report.
fn report_button(button: MouseButton) -> Option<emulator::MouseButton> {
    match button {
        MouseButton::Left => Some(emulator::MouseButton::Left),
        MouseButton::Middle => Some(emulator::MouseButton::Middle),
        MouseButton::Right => Some(emulator::MouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

pub struct TerminalPane {
    app: Option<Entity<AppState>>,
    session: Session,
    focus: FocusHandle,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    geometry: Option<Geometry>,
    /// The grid and cell size the emulator was last fitted to.
    fitted: Option<(Grid, (f32, f32))>,
    visible: bool,
    marked: Option<String>,
    drag: Option<Drag>,
    /// The button whose press went to the program, and the cell it last reported.
    reporting: Option<(MouseButton, (usize, usize))>,
    scroll_remainder: f32,
    theme: Option<ThemeColors>,
    resize_pending: bool,
    resize_settle: Option<Task<()>>,
    selection_menu: Option<SelectionMenu>,
    /// A selection menu waiting out a possible next click.
    selection_menu_delay: Option<Task<()>>,
    link_metadata: SharedMetadataCache,
    hover_pointer: Option<Point<Pixels>>,
    hovered_link: Option<LinkHit>,
    link_hover_candidate: Option<LinkHit>,
    link_hover_timer: Option<Task<()>>,
    link_hover_fetch: Option<Task<()>>,
    row_stability: Vec<RowStability>,
    stability_generation: u64,
    link_activation_request_id: u64,
    link_activation: Option<LinkActivation>,
    link_activation_task: Option<Task<()>>,
    _pump: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalPane {
    pub fn new(
        target: Target,
        client: ClientHandle,
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = colors::theme_colors(cx);
        let (session, mut signals) = Session::start(target, client, runtime, theme);
        let focus = cx.focus_handle();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(signal) = signals.receiver.recv().await {
                let alive = match signal {
                    Signal::Changed => {
                        signals.pending.store(false, std::sync::atomic::Ordering::Release);
                        this.update(cx, |pane, cx| {
                            pane.update_row_stability(cx);
                            cx.notify();
                        })
                        .is_ok()
                    }
                    Signal::Exited => this.update(cx, |_, cx| cx.emit(PaneEvent::Exited)).is_ok(),
                };
                if !alive {
                    return;
                }
            }
        });
        let subscriptions = vec![
            cx.on_focus(&focus, window, |pane, _, cx| pane.focus_changed(true, cx)),
            cx.on_blur(&focus, window, |pane, _, cx| pane.focus_changed(false, cx)),
            cx.observe_global::<TerminalFont>(|_, cx| cx.notify()),
        ];
        TerminalPane {
            app: None,
            session,
            focus,
            frame: Rc::default(),
            rows: Rc::default(),
            geometry: None,
            fitted: None,
            visible: true,
            marked: None,
            drag: None,
            reporting: None,
            scroll_remainder: 0.,
            theme: Some(theme),
            resize_pending: false,
            resize_settle: None,
            selection_menu: None,
            selection_menu_delay: None,
            link_metadata: Rc::new(RefCell::new((Instant::now(), t3_core::files::metadata::MetadataCache::default()))),
            hover_pointer: None,
            hovered_link: None,
            link_hover_candidate: None,
            link_hover_timer: None,
            link_hover_fetch: None,
            row_stability: Vec::new(),
            stability_generation: 0,
            link_activation_request_id: 0,
            link_activation: None,
            link_activation_task: None,
            _pump: pump,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn set_link_context(&mut self, app: Entity<AppState>, link_metadata: SharedMetadataCache) {
        self.app = Some(app);
        self.link_metadata = link_metadata;
    }

    pub fn terminal_id(&self) -> &str {
        &self.session.target.terminal_id
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if !visible {
            self.link_activation_request_id += 1;
            self.link_activation = None;
            self.link_activation_task = None;
            self.row_stability.clear();
            self.clear_hovered_link(cx);
        }
    }

    /// Called from the element's prepaint with the grid and cell size that fit this frame. The
    /// emulator is touched only when they change; the PTY hears the grid once it settles.
    pub(super) fn fit(&mut self, measured: Grid, cell: (f32, f32), cx: &mut Context<Self>) {
        if self.fitted == Some((measured, cell)) {
            return;
        }
        let regrid = self.fitted.is_none_or(|(grid, _)| grid != measured);
        self.fitted = Some((measured, cell));
        if regrid {
            self.link_activation_request_id += 1;
            self.link_activation_task = None;
        }
        self.session.update(|core| core.lay_out(measured, cell));
        if regrid {
            self.resize_pending = true;
            self.resize_settle = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_SETTLE).await;
                let _ = this.update(cx, |pane, _| pane.flush_resize());
            }));
        }
    }

    pub(super) fn set_geometry(&mut self, geometry: Geometry) {
        if self.geometry != Some(geometry) {
            self.link_activation_request_id += 1;
            self.link_activation_task = None;
        }
        self.geometry = Some(geometry);
    }

    /// Sends a grid still settling now, so input that follows reaches a shell with the settled
    /// width (`onWindowPointerRelease`).
    fn flush_resize(&mut self) {
        if std::mem::take(&mut self.resize_pending) {
            self.session.send_resize();
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if !focused {
            self.dismiss_selection_menu(cx);
            self.clear_hovered_link(cx);
        }
        let report = self.session.shared.lock().emulator.focus_input(focused);
        if let Some(report) = report {
            self.session.write(report);
        }
        cx.notify();
    }

    fn scroll_view(&mut self, change: impl FnOnce(&mut emulator::Emulator), cx: &mut Context<Self>) {
        self.session.update(|core| change(&mut core.emulator));
        self.update_row_stability(cx);
    }

    /// Sends input and follows the output again, like typing into any terminal does.
    fn input(&mut self, data: &str, cx: &mut Context<Self>) {
        self.flush_resize();
        let scrolled = {
            let core = self.session.shared.lock();
            if core.read_only {
                return;
            }
            core.emulator.display_offset() > 0
        };
        if scrolled {
            self.scroll_view(emulator::Emulator::scroll_to_bottom, cx);
        }
        let writes = self.session.write(data);
        if !writes.is_empty() {
            cx.spawn(async move |this, cx| {
                for write in writes {
                    if let Ok(Err(RpcError::Failure(failure))) = write.await {
                        let _ = this.update(cx, |pane, cx| pane.note(&failure.message, cx));
                        return;
                    }
                }
            })
            .detach();
        }
        if scrolled {
            cx.notify();
        }
    }

    fn note(&mut self, message: &str, cx: &mut Context<Self>) {
        self.session.update(|core| core.emulator.print_local(&terminal::system_message(message)));
        cx.notify();
    }

    /// The selection as an excerpt, when it holds more than blank lines (`readSelectionAction`).
    fn selected_excerpt(&self) -> Option<Excerpt> {
        let core = self.session.shared.lock();
        let text = normalize_text(&core.emulator.selection_text()?);
        let (start, end) = core.emulator.selection_rows()?;
        if text.is_empty() {
            return None;
        }
        let (line_start, line_end) = selection_line_range(start, end);
        Some(Excerpt { line_start, line_end, text })
    }

    /// `addSelectionToChat`: the selection goes to the composer and leaves the terminal.
    fn add_to_chat(&mut self, _: &AddToChat, _: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection_menu(cx);
        let Some(excerpt) = self.selected_excerpt() else { return };
        self.session.update(|core| core.emulator.clear_selection());
        cx.emit(PaneEvent::AddToChat(excerpt));
        cx.notify();
    }

    /// `showSelectionAction`: after a selecting gesture, its menu opens at the pointer, at once
    /// for a drag and once a multi-click settles.
    fn schedule_selection_menu(&mut self, at: Point<Pixels>, multi_click: bool, cx: &mut Context<Self>) {
        self.dismiss_selection_menu(cx);
        if self.selected_excerpt().is_none() {
            return;
        }
        if !multi_click {
            self.selection_menu = Some(SelectionMenu { at });
            cx.notify();
            return;
        }
        self.selection_menu_delay = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MULTI_CLICK_INTERVAL).await;
            let _ = this.update(cx, |pane, cx| {
                pane.selection_menu_delay = None;
                if pane.selected_excerpt().is_some() {
                    pane.selection_menu = Some(SelectionMenu { at });
                    cx.notify();
                }
            });
        }));
    }

    fn dismiss_selection_menu(&mut self, cx: &mut Context<Self>) {
        self.selection_menu_delay = None;
        if self.selection_menu.take().is_some() {
            cx.notify();
        }
    }

    fn render_selection_menu(&self, menu: &SelectionMenu, cx: &mut Context<Self>) -> gpui::AnyElement {
        gpui::deferred(
            gpui::anchored().position(menu.at).snap_to_window_with_margin(px(8.)).child(
                menu_surface(cx)
                    .occlude()
                    .on_mouse_down_out(cx.listener(|pane, _, _, cx| pane.dismiss_selection_menu(cx)))
                    .child(
                        MenuItem::new("terminal-selection-add-to-chat", "Add to chat")
                            .on_click(cx.listener(|pane, _, window, cx| pane.add_to_chat(&AddToChat, window, cx))),
                    )
                    .child(MenuItem::new("terminal-selection-copy", "Copy").on_click(cx.listener(|pane, _, window, cx| {
                        pane.dismiss_selection_menu(cx);
                        pane.copy(&Copy, window, cx);
                        window.focus(&pane.focus, cx);
                    }))),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.session.shared.lock().emulator.selection_text();
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        if text.is_empty() {
            return;
        }
        let bracketed = self.session.shared.lock().emulator.mode().contains(TermMode::BRACKETED_PASTE);
        self.input(&encode_paste(&text, bracketed), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.link_activation_request_id += 1;
        self.link_activation_task = None;
        self.session.update(|core| core.emulator.select_all());
        cx.notify();
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        self.input("\x0c", cx);
    }

    fn jump_to_latest(&mut self, _: &JumpToLatest, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_view(emulator::Emulator::scroll_to_bottom, cx);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection_menu(cx);
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        let key = KeyInput {
            key: &keystroke.key,
            key_char: keystroke.key_char.as_deref(),
            control: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            platform: modifiers.platform,
        };
        let (app_cursor, has_selection, alt_screen, rows) = {
            let core = self.session.shared.lock();
            let mode = core.emulator.mode();
            (
                mode.contains(TermMode::APP_CURSOR),
                core.emulator.has_selection(),
                mode.contains(TermMode::ALT_SCREEN),
                core.emulator.grid().rows as i32,
            )
        };
        let mapper = cx.keyboard_mapper().clone();
        if let Some(shortcut) = local_shortcut(&key, MAC, has_selection, alt_screen, mapper.get_key_equivalents()) {
            cx.stop_propagation();
            match shortcut {
                TerminalShortcut::Copy { clear_selection } => {
                    self.copy(&Copy, window, cx);
                    if clear_selection {
                        self.session.update(|core| core.emulator.clear_selection());
                        cx.notify();
                    }
                }
                TerminalShortcut::Paste => self.paste(&Paste, window, cx),
                TerminalShortcut::SelectAll => self.select_all(&SelectAll, window, cx),
                TerminalShortcut::ScrollPages(pages) => {
                    self.scroll_view(|emulator| emulator.scroll(pages * rows.max(1)), cx);
                    cx.notify();
                }
                TerminalShortcut::ScrollToTop => {
                    self.scroll_view(emulator::Emulator::scroll_to_top, cx);
                    cx.notify();
                }
                TerminalShortcut::ScrollToBottom => {
                    self.scroll_view(emulator::Emulator::scroll_to_bottom, cx);
                    cx.notify();
                }
            }
            return;
        }
        let data = match shortcut_data(&key) {
            Some(data) => data.to_owned(),
            None => match encode_key(&key, app_cursor) {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => return,
            },
        };
        cx.stop_propagation();
        self.input(&data, cx);
    }

    /// The visible cell under a pointer, clamped to the grid.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let geometry = self.geometry?;
        Some(hit_cell(
            f32::from(position.x - geometry.origin.x),
            f32::from(position.y - geometry.origin.y),
            geometry.cell_width,
            geometry.line_height,
            geometry.cols,
            geometry.rows,
        ))
    }

    fn hit(&self, position: Point<Pixels>) -> Option<(GridPoint, Side)> {
        let (row, col, side) = self.cell_at(position)?;
        Some((self.session.shared.lock().emulator.point_at(row, col), side))
    }

    /// The report a pointer event sends to a program tracking the mouse, if it sends one.
    fn mouse_report(&self, action: MouseAction, button: emulator::MouseButton, alt: bool, position: Point<Pixels>) -> Option<(String, (usize, usize))> {
        let (row, col, _) = self.cell_at(position)?;
        let core = self.session.shared.lock();
        if core.read_only {
            return None;
        }
        Some((core.emulator.mouse_report(action, button, alt, row, col)?, (row, col)))
    }

    fn link_cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let geometry = self.geometry?;
        if geometry.cols == 0 || geometry.rows == 0 || geometry.cell_width <= 0. || geometry.line_height <= 0. {
            return None;
        }
        let dx = f32::from(position.x - geometry.origin.x);
        let dy = f32::from(position.y - geometry.origin.y);
        if dx < 0. || dy < 0. {
            return None;
        }
        let col = (dx / geometry.cell_width).floor() as usize;
        let row = (dy / geometry.line_height).floor() as usize;
        (col < geometry.cols && row < geometry.rows).then_some((row, col))
    }

    fn link_query_path(&self, text: &str) -> Option<String> {
        let cwd = &self.session.target.location.cwd;
        let target = links::resolve_link_target(text, cwd);
        metadata::query_path(&target, Some(cwd))
    }

    fn has_link_scopes(&self, require_filesystem_read: bool, cx: &App) -> bool {
        let Some(app) = &self.app else { return false };
        let Some(client) = app.read(cx).client_handle() else { return false };
        let session = client.session_state();
        let state = session.borrow();
        let Some(auth) = state.as_ref() else { return false };
        auth.grants(ORCHESTRATION_OPERATE_SCOPE) && (!require_filesystem_read || auth.grants(FILESYSTEM_READ_SCOPE))
    }

    fn can_activate_link(&self, text: &str, cx: &App) -> bool {
        if links::is_url(text) {
            return true;
        }
        if !self.has_link_scopes(true, cx) {
            return false;
        }
        let Some(query) = self.link_query_path(text) else {
            return false;
        };
        let mut cache = self.link_metadata.borrow_mut();
        let now = cache.0.elapsed().as_millis() as u64;
        let thread = self.session.target.thread_id.as_str();
        matches!(cache.1.lookup(&query, Some(thread), now), Lookup::Known(Some(meta)) if meta.is_openable())
    }

    fn link_at_position(&self, position: Point<Pixels>, allow_validation: bool, cx: &App) -> Option<LinkHit> {
        let (row, col) = self.link_cell_at(position)?;
        let hit = self.session.shared.lock().emulator.link_at(row, col)?;
        if links::is_url(&hit.text) {
            return Some(hit);
        }
        if !self.has_link_scopes(true, cx) {
            return None;
        }
        let query = self.link_query_path(&hit.text)?;
        let mut cache = self.link_metadata.borrow_mut();
        let now = cache.0.elapsed().as_millis() as u64;
        let thread = self.session.target.thread_id.as_str();
        match cache.1.lookup(&query, Some(thread), now) {
            Lookup::Known(Some(meta)) if meta.is_openable() => Some(hit),
            Lookup::Known(_) => None,
            Lookup::Fetch if allow_validation => Some(hit),
            Lookup::Fetch => None,
        }
    }

    fn validate_link(&self, text: &str, cx: &mut Context<Self>) -> LinkValidation {
        if self.can_activate_link(text, cx) {
            return LinkValidation::Ready(true);
        }
        if !self.has_link_scopes(true, cx) {
            return LinkValidation::Ready(false);
        }
        let Some(query) = self.link_query_path(text) else {
            return LinkValidation::Ready(false);
        };
        {
            let mut cache = self.link_metadata.borrow_mut();
            let now = cache.0.elapsed().as_millis() as u64;
            let thread = self.session.target.thread_id.as_str();
            if let Lookup::Known(entry) = cache.1.lookup(&query, Some(thread), now) {
                return LinkValidation::Ready(entry.is_some_and(|meta| meta.is_openable()));
            }
        }
        let Some(app) = &self.app else {
            return LinkValidation::Ready(false);
        };
        let Some(client) = app.read(cx).client_handle() else {
            return LinkValidation::Ready(false);
        };
        let runtime = app.read(cx).runtime().clone();
        let link_metadata = self.link_metadata.clone();
        let thread_id = self.session.target.thread_id.clone();
        let task = runtime.spawn(async move {
            let Ok(reply) = client.request(GET_METADATA, metadata::request(std::slice::from_ref(&query))).await else {
                return None;
            };
            let entry = metadata::decode(&reply, 1).ok()?.into_iter().next().flatten();
            Some((query, entry))
        });
        LinkValidation::Pending(cx.spawn(async move |_, _| {
            let Ok(Some((query, entry))) = task.await else {
                return false;
            };
            let openable = entry.as_ref().is_some_and(|meta| meta.is_openable());
            let mut cache = link_metadata.borrow_mut();
            let now = cache.0.elapsed().as_millis() as u64;
            cache.1.remember(&query, entry, Some(&thread_id), now);
            openable
        }))
    }

    fn update_row_stability(&mut self, cx: &mut Context<Self>) {
        let mut frame = self.frame.borrow_mut();
        super::session::take_newer(&self.session.front, &mut frame);
        if frame.generation == self.stability_generation {
            return;
        }
        self.stability_generation = frame.generation;
        let snapshot = &frame.snapshot;
        let now = Instant::now();
        let mut next = Vec::with_capacity(snapshot.rows);
        for row in 0..snapshot.rows {
            let mut hasher = DefaultHasher::new();
            snapshot.cols.hash(&mut hasher);
            let start = row * snapshot.cols;
            for (offset, cell) in snapshot.row(row).iter().enumerate() {
                cell.c.hash(&mut hasher);
                if cell.combining {
                    snapshot.combining(start + offset).for_each(|c| c.hash(&mut hasher));
                }
            }
            let hash = hasher.finish();
            let changed_at = self
                .row_stability
                .get(row)
                .filter(|prev| prev.hash == hash)
                .map_or(now, |prev| prev.changed_at);
            next.push(RowStability { hash, changed_at });
        }
        self.row_stability = next;
        drop(frame);
        self.refresh_hovered_link(cx);
    }

    fn clear_hovered_link(&mut self, cx: &mut Context<Self>) {
        self.hover_pointer = None;
        self.update_link_hover(None, cx);
        self.set_hovered_link(None, cx);
    }

    fn refresh_hovered_link(&mut self, cx: &mut Context<Self>) {
        let link = self
            .hover_pointer
            .and_then(|pointer| self.link_cell_at(pointer))
            .and_then(|(row, col)| self.session.shared.lock().emulator.link_at(row, col));
        self.update_link_hover(link.clone(), cx);
        let active = link.filter(|hit| self.can_activate_link(&hit.text, cx));
        self.set_hovered_link(active, cx);
    }

    fn update_link_hover(&mut self, link: Option<LinkHit>, cx: &mut Context<Self>) {
        if link == self.link_hover_candidate {
            return;
        }
        self.link_hover_candidate = link.clone();
        self.link_hover_timer = None;
        self.link_hover_fetch = None;
        let Some(link) = link else { return };
        if links::is_url(&link.text) || !self.has_link_scopes(true, cx) {
            return;
        }
        let Some(query) = self.link_query_path(&link.text) else { return };
        {
            let mut cache = self.link_metadata.borrow_mut();
            let now = cache.0.elapsed().as_millis() as u64;
            let thread = self.session.target.thread_id.as_str();
            if !matches!(cache.1.lookup(&query, Some(thread), now), Lookup::Fetch) {
                return;
            }
        }
        let now = Instant::now();
        let mut changed_at = None::<Instant>;
        for y in link.start.0..=link.end.0 {
            let row_at = self.row_stability.get(y).map_or(now, |s| s.changed_at);
            changed_at = Some(changed_at.map_or(row_at, |acc| acc.max(row_at)));
        }
        let elapsed = changed_at.unwrap_or(now).elapsed();
        if elapsed >= LINK_HOVER_DELAY {
            self.fetch_hovered_link_metadata(query, cx);
        } else {
            let wait = LINK_HOVER_DELAY.saturating_sub(elapsed);
            self.link_hover_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |pane, cx| {
                    pane.link_hover_timer = None;
                    pane.fetch_hovered_link_metadata(query, cx);
                });
            }));
        }
    }

    fn fetch_hovered_link_metadata(&mut self, query: String, cx: &mut Context<Self>) {
        if !self.has_link_scopes(true, cx) {
            return;
        }
        let Some(app) = &self.app else { return };
        let Some(client) = app.read(cx).client_handle() else { return };
        let runtime = app.read(cx).runtime().clone();
        let link_metadata = self.link_metadata.clone();
        let thread_id = self.session.target.thread_id.clone();
        let task = runtime.spawn(async move {
            let reply = client.request(GET_METADATA, metadata::request(std::slice::from_ref(&query))).await.ok()?;
            let entry = metadata::decode(&reply, 1).ok()?.into_iter().next().flatten();
            Some((query, entry))
        });
        self.link_hover_fetch = Some(cx.spawn(async move |this, cx| {
            let Ok(Some((query, entry))) = task.await else { return };
            {
                let mut cache = link_metadata.borrow_mut();
                let now = cache.0.elapsed().as_millis() as u64;
                cache.1.remember(&query, entry, Some(&thread_id), now);
            }
            let _ = this.update(cx, |pane, cx| {
                pane.link_hover_fetch = None;
                pane.refresh_hovered_link(cx);
            });
        }));
    }

    fn set_hovered_link(&mut self, link: Option<LinkHit>, cx: &mut Context<Self>) {
        if self.hovered_link == link {
            return;
        }
        self.hovered_link = link;
        cx.notify();
    }

    fn activate_link(
        &mut self,
        link: LinkHit,
        validation: LinkValidation,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        match validation {
            LinkValidation::Ready(true) => self.open_validated_link(&link.text, modifiers, cx),
            LinkValidation::Ready(false) => {
                self.session.update(|core| core.emulator.clear_selection());
                cx.notify();
            }
            LinkValidation::Pending(task) => {
                let request_id = self.link_activation_request_id;
                let geometry = self.geometry;
                self.link_activation_task = Some(cx.spawn(async move |this, cx| {
                    let valid = task.await;
                    let _ = this.update(cx, |pane, cx| {
                        pane.link_activation_task = None;
                        if !pane.visible
                            || request_id != pane.link_activation_request_id
                            || pane.geometry != geometry
                            || pane.link_cell_at(position).and_then(|(row, col)| pane.session.shared.lock().emulator.link_at(row, col)).as_ref() != Some(&link)
                        {
                            return;
                        }
                        if valid {
                            pane.open_validated_link(&link.text, modifiers, cx);
                        } else {
                            pane.session.update(|core| core.emulator.clear_selection());
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }

    fn open_validated_link(&mut self, text: &str, modifiers: Modifiers, cx: &mut Context<Self>) {
        if links::is_url(text) {
            let force_browser = modifiers.platform || modifiers.control;
            let can_preview = self
                .app
                .as_ref()
                .and_then(|app| app.read(cx).client_handle())
                .is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|auth| auth.grants(PREVIEW_OPERATE_SCOPE)));
            let target = crate::settings::client_settings(cx).browser_link_target();
            if !force_browser
                && can_preview
                && target == BrowserLinkTarget::App
                && let Some(previews) = crate::preview::previews(cx)
            {
                let thread_id = self.session.target.thread_id.clone();
                previews.update(cx, |previews, cx| previews.open_link(thread_id, text.to_owned(), cx));
                return;
            }
            cx.open_url(text);
            return;
        }
        if !self.has_link_scopes(false, cx) {
            return;
        }
        let Some(app) = &self.app else { return };
        let target = links::resolve_link_target(text, &self.session.target.location.cwd);
        let project_root = self
            .session
            .target
            .location
            .worktree_path
            .as_deref()
            .unwrap_or(&self.session.target.location.cwd)
            .to_owned();
        let request = app.update(cx, |app, _| {
            let available = app
                .server_config
                .config
                .as_ref()
                .map(|config| config.available_editors.clone())
                .unwrap_or_default();
            let (editor, _) = preferred_editor(&available, None)?;
            Some(app.rpc(OPEN_IN_EDITOR, open_in_editor_request_with_root(&target, editor, Some(&project_root))))
        });
        let Some(request) = request else {
            self.note("No editor is available to open this path.", cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Err(error) = request.await {
                let _ = this.update(cx, |pane, cx| pane.note(&error.to_string(), cx));
            }
        })
        .detach();
    }

    fn begin_selection(&mut self, position: Point<Pixels>, click_count: usize, extend: bool, cx: &mut Context<Self>) {
        let Some((point, side)) = self.hit(position) else { return };
        let armed = self.session.update(|core| match click_count {
            0 => None,
            1 if extend && core.emulator.has_selection() => {
                core.emulator.update_selection(point, side);
                Some(true)
            }
            1 => {
                core.emulator.clear_selection();
                Some(false)
            }
            2 => {
                core.emulator.start_selection(SelectionType::Semantic, point, side);
                Some(true)
            }
            _ => {
                core.emulator.start_selection(SelectionType::Lines, point, side);
                Some(true)
            }
        });
        let Some(armed) = armed else { return };
        self.drag = Some(Drag { origin: position, armed });
        cx.notify();
    }

    /// Capture phase, so a press the program takes never starts a selection or the context menu.
    fn report_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.link_activation_request_id += 1;
        self.link_activation_task = None;
        let Some(button) = report_button(event.button).filter(|_| reports_mouse(&event.modifiers)) else { return };
        let Some((report, cell)) = self.mouse_report(MouseAction::Press, button, event.modifiers.alt, event.position) else { return };
        cx.stop_propagation();
        self.clear_hovered_link(cx);
        window.focus(&self.focus, cx);
        cx.emit(PaneEvent::Activated);
        self.reporting = Some((event.button, cell));
        self.input(&report, cx);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.link_activation_request_id += 1;
        self.link_activation_task = None;
        window.focus(&self.focus, cx);
        cx.emit(PaneEvent::Activated);
        if event.button == MouseButton::Left
            && !event.modifiers.shift
            && event.click_count == 1
            && let Some(link) = self.link_at_position(event.position, true, cx)
        {
            cx.stop_propagation();
            let validation = self.validate_link(&link.text, cx);
            self.link_activation = Some(LinkActivation { origin: event.position, link, validation });
            return;
        }
        self.clear_hovered_link(cx);
        self.begin_selection(event.position, event.click_count, event.modifiers.shift, cx);
    }

    /// Drags with a reported button, and hover motion for programs that track all motion
    /// (DEC 1003), go to the program once per cell.
    fn report_motion(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) -> bool {
        let held = self.reporting;
        if held.is_none() && !reports_mouse(&event.modifiers) {
            return false;
        }
        let button = held.and_then(|(button, _)| report_button(button)).unwrap_or(emulator::MouseButton::None);
        let Some((report, cell)) = self.mouse_report(MouseAction::Motion, button, event.modifiers.alt, event.position) else {
            return held.is_some();
        };
        if let Some((button, last)) = held {
            if last == cell {
                return true;
            }
            self.reporting = Some((button, cell));
        }
        self.set_hovered_link(None, cx);
        self.input(&report, cx);
        true
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = &self.link_activation {
            let travel = event.position - active.origin;
            if f32::from(travel.x).hypot(f32::from(travel.y)) <= LINK_DRAG_THRESHOLD {
                return;
            }
            let origin = active.origin;
            self.link_activation = None;
            self.clear_hovered_link(cx);
            self.begin_selection(origin, 1, false, cx);
        }
        if self.report_motion(event, cx) {
            return;
        }
        let Some(mut drag) = self.drag.filter(|_| event.pressed_button == Some(MouseButton::Left)) else {
            self.hover_pointer = Some(event.position);
            self.refresh_hovered_link(cx);
            return;
        };
        self.clear_hovered_link(cx);
        if !drag.armed {
            let travel = event.position - drag.origin;
            if f32::from(travel.x).hypot(f32::from(travel.y)) < DRAG_THRESHOLD {
                return;
            }
            let Some((anchor, side)) = self.hit(drag.origin) else { return };
            self.session.update(|core| core.emulator.start_selection(SelectionType::Simple, anchor, side));
            drag.armed = true;
            self.drag = Some(drag);
        }
        if let Some((point, side)) = self.hit(event.position) {
            self.session.update(|core| core.emulator.update_selection(point, side));
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.flush_resize();
        if let Some(active) = self.link_activation.take().filter(|_| event.button == MouseButton::Left) {
            if self.link_at_position(event.position, true, cx).as_ref() == Some(&active.link) {
                self.activate_link(active.link, active.validation, event.position, event.modifiers, cx);
            }
            return;
        }
        if let Some((button, _)) = self.reporting.filter(|(button, _)| *button == event.button) {
            self.reporting = None;
            let report = report_button(button)
                .and_then(|button| self.mouse_report(MouseAction::Release, button, event.modifiers.alt, event.position));
            if let Some((report, _)) = report {
                self.input(&report, cx);
            }
            self.hover_pointer = Some(event.position);
            self.refresh_hovered_link(cx);
            return;
        }
        if event.button == MouseButton::Left {
            let selecting = self.drag.take().is_some_and(|drag| drag.armed);
            if selecting {
                self.schedule_selection_menu(event.position, event.click_count >= 2, cx);
            }
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection_menu(cx);
        self.link_activation_request_id += 1;
        self.link_activation_task = None;
        let Some(geometry) = self.geometry else { return };
        let delta = event.delta.pixel_delta(px(geometry.line_height)).y;
        self.scroll_remainder += f32::from(delta) / geometry.line_height;
        let lines = self.scroll_remainder.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32;
        let Some((row, col, _)) = self.cell_at(event.position) else { return };
        let input = {
            let core = self.session.shared.lock();
            if core.read_only { None } else { core.emulator.wheel_input(lines, row, col, reports_mouse(&event.modifiers)) }
        };
        cx.stop_propagation();
        match input {
            Some(input) => self.input(&input, cx),
            None => {
                self.scroll_view(|emulator| emulator.scroll(lines), cx);
                cx.notify();
            }
        }
    }

    fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let cursor = self.frame.borrow().snapshot.cursor?;
        Some(Bounds::new(
            gpui::point(
                geometry.origin.x + px(geometry.cell_width * cursor.col as f32),
                geometry.origin.y + px(geometry.line_height * cursor.row as f32),
            ),
            gpui::size(px(geometry.cell_width), px(geometry.line_height)),
        ))
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = colors::theme_colors(cx);
        if self.theme != Some(theme) {
            self.session.update(|core| core.emulator.set_theme(theme));
            self.theme = Some(theme);
        }
        let paint = colors::paint(cx);
        let focused = self.focus.is_focused(window);
        let shared = self.session.shared.clone();
        let menu_focus = self.focus.clone();
        let element = TerminalElement::new(
            cx.entity(),
            self.session.front.clone(),
            self.frame.clone(),
            self.rows.clone(),
            self.focus.clone(),
            focused,
            self.marked.clone().map(SharedString::from),
        )
        .hovered_link(self.hovered_link.clone());
        div()
            .id(SharedString::from(format!("terminal-pane-{}", self.terminal_id())))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .bg(paint.background)
            .when(self.hovered_link.is_some(), |this| this.cursor_pointer())
            .when(self.hovered_link.is_none(), |this| this.cursor_text())
            .on_hover(cx.listener(|pane, hovered: &bool, _, cx| {
                if !*hovered {
                    pane.clear_hovered_link(cx);
                }
            }))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::jump_to_latest))
            .on_action(cx.listener(Self::add_to_chat))
            .on_key_down(cx.listener(Self::key_down))
            .capture_any_mouse_down(cx.listener(Self::report_mouse_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(element)
            .children(self.selection_menu.as_ref().map(|menu| self.render_selection_menu(menu, cx)))
            .context_menu(move |menu, _, _| {
                let core = shared.lock();
                let selected = core.emulator.selection_text().is_some_and(|text| !normalize_text(&text).is_empty());
                let read_only = core.read_only;
                drop(core);
                menu.action_context(menu_focus.clone())
                    .menu_with_disabled("Add to chat", Box::new(AddToChat), !selected)
                    .menu_with_disabled("Copy", Box::new(Copy), !selected)
                    .menu_with_disabled("Paste", Box::new(Paste), read_only)
                    .menu("Select all", Box::new(SelectAll))
                    .menu("Jump to latest", Box::new(JumpToLatest))
            })
    }
}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked.as_ref()?;
        let units: Vec<u16> = marked.encode_utf16().collect();
        let range = range.start.min(units.len())..range.end.min(units.len());
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(&units[range]))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let end = self.marked.as_ref().map_or(0, |marked| marked.encode_utf16().count());
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|marked| 0..marked.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let had_marked = self.marked.take().is_some();
        if !text.is_empty() {
            self.input(text, cx);
        }
        if had_marked {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(&mut self, _: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        self.cursor_bounds()
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.session.shared.lock().read_only
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use serde_json::json;
    use t3_core::files::metadata::MetadataCache;
    use t3_core::terminal::LaunchLocation;

    fn test_pane(
        cx: &mut gpui::TestAppContext,
        runtime: &tokio::runtime::Runtime,
        cols: u16,
        rows: u16,
    ) -> (Entity<AppState>, t3_client::supervisor::ClientFixture, gpui::WindowHandle<TerminalPane>) {
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:operate", "filesystem:read"]
                }))
                .unwrap(),
            ))
            .unwrap();
        let target = Target {
            thread_id: "menu-thread".into(),
            terminal_id: "term-1".into(),
            location: LaunchLocation::new("/repo", None),
        };
        let client = state.read_with(cx, |state, _| state.client_handle().unwrap());
        let cache = Rc::new(RefCell::new((Instant::now(), MetadataCache::default())));
        let window = cx.add_window(|window, cx| {
            let mut pane = TerminalPane::new(target, client, runtime.handle().clone(), window, cx);
            pane.set_link_context(state.clone(), cache);
            pane.set_geometry(Geometry {
                origin: Point::default(),
                cell_width: 10.,
                line_height: 20.,
                cols: cols as usize,
                rows: rows as usize,
            });
            pane.session.update(|core| core.emulator.resize(Grid::new(cols, rows)));
            pane
        });
        (state, fixture, window)
    }

    #[gpui::test]
    fn scroll_refreshes_hovered_link_and_row_stability(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture, window) = test_pane(cx, &runtime, 40, 4);
        window
            .update(cx, |pane, _, cx| {
                pane.set_geometry(Geometry {
                    origin: Point::default(),
                    cell_width: 10.,
                    line_height: 20.,
                    cols: 40,
                    rows: 4,
                });
                pane.session.update(|core| {
                    core.emulator.resize(Grid::new(40, 4));
                    core.emulator.feed(b"https://example.com\r\nline1\r\nline2\r\nline3\r\nline4");
                    assert_eq!(core.emulator.display_offset(), 0);
                });
                // Row 0 of the viewport currently shows "line1", while "https://example.com" scrolled into history.
                pane.scroll_view(|emulator| emulator.scroll(1), cx);
                assert_eq!(pane.session.shared.lock().emulator.display_offset(), 1);
                pane.hover_pointer = Some(gpui::point(px(25.), px(10.)));
                pane.refresh_hovered_link(cx);
                assert_eq!(pane.hovered_link.as_ref().map(|hit| hit.text.as_str()), Some("https://example.com"));
                let before_gen = pane.stability_generation;
                // Scrolling back down moves the URL off row 0; hover and stability update immediately.
                pane.scroll_view(|emulator| emulator.scroll(-1), cx);
                assert_eq!(pane.session.shared.lock().emulator.display_offset(), 0);
                assert!(pane.stability_generation > before_gen);
                assert_eq!(pane.hovered_link, None);
            })
            .unwrap();
        state.update(cx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    fn cached_miss_click_clears_selection_without_activating(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture, window) = test_pane(cx, &runtime, 40, 4);
        window
            .update(cx, |pane, window, cx| {
                {
                    let mut cache = pane.link_metadata.borrow_mut();
                    let now = cache.0.elapsed().as_millis() as u64;
                    cache.1.retain_thread("menu-thread");
                    cache.1.remember("/repo/src/missing.rs", None, Some("menu-thread"), now);
                }
                pane.session.update(|core| {
                    core.emulator.feed(b"open src/missing.rs now");
                    core.emulator.select_all();
                });
                assert!(pane.session.shared.lock().emulator.has_selection());
                let pos = gpui::point(px(75.), px(10.));
                pane.mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: pos,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                pane.mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: pos,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    },
                    window,
                    cx,
                );
                assert!(!pane.session.shared.lock().emulator.has_selection());
            })
            .unwrap();
        let commands = fixture.take_commands();
        assert!(
            !commands.iter().any(|method| *method == GET_METADATA || *method == OPEN_IN_EDITOR),
            "cached miss click must not fetch metadata or open editor, got {commands:?}"
        );
        state.update(cx, |state, _| state.stop_pump());
    }
}
