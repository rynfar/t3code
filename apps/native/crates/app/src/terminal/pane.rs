//! A terminal pane: one session painted by `TerminalElement`, with keyboard and IME input,
//! selection, scrollback, the clipboard, and mouse reports for programs that track the mouse.
//! Ports `TerminalSessionViewport` in `apps/web/src/components/ThreadTerminalDrawer.tsx` (its
//! context menu, editing shortcuts and exit handling) and the pointer handling of
//! `apps/web/src/terminal/ghostty/surface.ts`.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Styled, Subscription, Task, UTF16Selection, Window, div, px,
};
use gpui_component::menu::ContextMenuExt;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::terminal::emulator::{self, GridPoint, MouseAction, SelectionType, Side, TermMode, ThemeColors, hit_cell};
use t3_core::terminal::{self, Grid, KeyInput, encode_key, encode_paste, shortcut_data};

use super::colors;
use super::element::{RowCache, TerminalElement, TerminalFont};
use super::session::{Frame, Session, Signal, Target};
use super::{Clear, Copy, JumpToLatest, Paste, SelectAll};

/// The PTY hears a new grid only after the size settles, so a drag does not reflow the prompt.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
const DRAG_THRESHOLD: f32 = 2.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell_width: f32,
    pub line_height: f32,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneEvent {
    /// The shell ended while attached.
    Exited,
    /// The user clicked into this pane.
    Activated,
}

#[derive(Clone, Copy)]
struct Drag {
    origin: Point<Pixels>,
    armed: bool,
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
    session: Session,
    focus: FocusHandle,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    geometry: Option<Geometry>,
    /// The grid and cell size the emulator was last fitted to.
    fitted: Option<(Grid, (f32, f32))>,
    marked: Option<String>,
    drag: Option<Drag>,
    /// The button whose press went to the program, and the cell it last reported.
    reporting: Option<(MouseButton, (usize, usize))>,
    scroll_remainder: f32,
    theme: Option<ThemeColors>,
    resize_pending: bool,
    resize_settle: Option<Task<()>>,
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
                        this.update(cx, |_, cx| cx.notify()).is_ok()
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
            session,
            focus,
            frame: Rc::default(),
            rows: Rc::default(),
            geometry: None,
            fitted: None,
            marked: None,
            drag: None,
            reporting: None,
            scroll_remainder: 0.,
            theme: Some(theme),
            resize_pending: false,
            resize_settle: None,
            _pump: pump,
            _subscriptions: subscriptions,
        }
    }

    pub fn terminal_id(&self) -> &str {
        &self.session.target.terminal_id
    }

    /// Called from the element's prepaint with the grid and cell size that fit this frame. The
    /// emulator is touched only when they change; the PTY hears the grid once it settles.
    pub(super) fn fit(&mut self, measured: Grid, cell: (f32, f32), cx: &mut Context<Self>) {
        if self.fitted == Some((measured, cell)) {
            return;
        }
        let regrid = self.fitted.is_none_or(|(grid, _)| grid != measured);
        self.fitted = Some((measured, cell));
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
        let report = self.session.shared.lock().emulator.focus_input(focused);
        if let Some(report) = report {
            self.session.write(report);
        }
        cx.notify();
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
            self.session.update(|core| core.emulator.scroll_to_bottom());
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
        self.session.update(|core| core.emulator.select_all());
        cx.notify();
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        self.input("\x0c", cx);
    }

    fn jump_to_latest(&mut self, _: &JumpToLatest, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(|core| core.emulator.scroll_to_bottom());
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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
        let app_cursor = self.session.shared.lock().emulator.mode().contains(TermMode::APP_CURSOR);
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

    /// Capture phase, so a press the program takes never starts a selection or the context menu.
    fn report_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(button) = report_button(event.button).filter(|_| reports_mouse(&event.modifiers)) else { return };
        let Some((report, cell)) = self.mouse_report(MouseAction::Press, button, event.modifiers.alt, event.position) else { return };
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        cx.emit(PaneEvent::Activated);
        self.reporting = Some((event.button, cell));
        self.input(&report, cx);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.emit(PaneEvent::Activated);
        let Some((point, side)) = self.hit(event.position) else { return };
        let (click_count, extend) = (event.click_count, event.modifiers.shift);
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
        self.drag = Some(Drag { origin: event.position, armed });
        cx.notify();
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
        self.input(&report, cx);
        true
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.report_motion(event, cx) {
            return;
        }
        let Some(mut drag) = self.drag.filter(|_| event.pressed_button == Some(MouseButton::Left)) else { return };
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
        if let Some((button, _)) = self.reporting.filter(|(button, _)| *button == event.button) {
            self.reporting = None;
            let report = report_button(button)
                .and_then(|button| self.mouse_report(MouseAction::Release, button, event.modifiers.alt, event.position));
            if let Some((report, _)) = report {
                self.input(&report, cx);
            }
            return;
        }
        if event.button == MouseButton::Left {
            self.drag = None;
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
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
                self.session.update(|core| core.emulator.scroll(lines));
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
        );
        div()
            .id(SharedString::from(format!("terminal-pane-{}", self.terminal_id())))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .bg(paint.background)
            .cursor_text()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::jump_to_latest))
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
            .context_menu(move |menu, _, _| {
                let core = shared.lock();
                let (selected, read_only) = (core.emulator.has_selection(), core.read_only);
                drop(core);
                menu.action_context(menu_focus.clone())
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
