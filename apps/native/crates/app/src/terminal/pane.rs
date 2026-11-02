//! A terminal pane: one session painted by `TerminalElement`, with keyboard and IME input,
//! selection, scrollback and the clipboard. Ports `TerminalSessionViewport` in
//! `apps/web/src/components/ThreadTerminalDrawer.tsx`: its context menu (Copy, Paste, Select
//! all, Jump to latest), the editing shortcuts, and the exit handling.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Styled, Subscription, Task, UTF16Selection, Window, div, px,
};
use gpui_component::menu::ContextMenuExt;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::terminal::emulator::{GridPoint, SelectionType, Side, Snapshot, TermMode, ThemeColors, hit_cell};
use t3_core::terminal::{self, Grid, KeyInput, encode_key, encode_paste, shortcut_data};

use super::colors;
use super::element::TerminalElement;
use super::session::{Session, Signal, Target};
use super::{Clear, Copy, JumpToLatest, Paste, SelectAll};

/// The pane repaints at most once per frame however fast output arrives.
const FRAME: Duration = Duration::from_millis(16);
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

pub struct TerminalPane {
    session: Session,
    focus: FocusHandle,
    frame: Rc<RefCell<Snapshot>>,
    geometry: Option<Geometry>,
    marked: Option<String>,
    drag: Option<Drag>,
    scroll_remainder: f32,
    theme: Option<ThemeColors>,
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
                        let alive = this.update(cx, |_, cx| cx.notify()).is_ok();
                        cx.background_executor().timer(FRAME).await;
                        alive
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
        ];
        TerminalPane {
            session,
            focus,
            frame: Rc::default(),
            geometry: None,
            marked: None,
            drag: None,
            scroll_remainder: 0.,
            theme: Some(theme),
            resize_settle: None,
            _pump: pump,
            _subscriptions: subscriptions,
        }
    }

    pub fn terminal_id(&self) -> &str {
        &self.session.target.terminal_id
    }

    pub fn is_read_only(&self) -> bool {
        self.session.shared.lock().read_only
    }

    /// Called from the element's prepaint with this frame's fit.
    pub(super) fn laid_out(&mut self, measured: Grid, geometry: Geometry, cx: &mut Context<Self>) {
        self.geometry = Some(geometry);
        let changed = {
            let mut core = self.session.shared.lock();
            let changed = core.measured != Some(measured);
            core.measured = Some(measured);
            changed
        };
        if changed {
            self.resize_settle = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_SETTLE).await;
                let _ = this.update(cx, |pane, _| pane.session.send_resize());
            }));
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
        let scrolled = {
            let mut core = self.session.shared.lock();
            if core.read_only {
                return;
            }
            let scrolled = core.emulator.display_offset() > 0;
            core.emulator.scroll_to_bottom();
            scrolled
        };
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
        self.session.shared.lock().emulator.print_local(&terminal::system_message(message));
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
        self.session.shared.lock().emulator.select_all();
        cx.notify();
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        self.input("\x0c", cx);
    }

    fn jump_to_latest(&mut self, _: &JumpToLatest, _: &mut Window, cx: &mut Context<Self>) {
        self.session.shared.lock().emulator.scroll_to_bottom();
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

    fn hit(&self, position: Point<Pixels>) -> Option<(GridPoint, Side)> {
        let geometry = self.geometry?;
        let (row, col, side) = hit_cell(
            f32::from(position.x - geometry.origin.x),
            f32::from(position.y - geometry.origin.y),
            geometry.cell_width,
            geometry.line_height,
            geometry.cols,
            geometry.rows,
        );
        Some((self.session.shared.lock().emulator.point_at(row, col), side))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.emit(PaneEvent::Activated);
        let Some((point, side)) = self.hit(event.position) else { return };
        let mut core = self.session.shared.lock();
        let armed = match event.click_count {
            0 => return,
            1 if event.modifiers.shift && core.emulator.has_selection() => {
                core.emulator.update_selection(point, side);
                true
            }
            1 => {
                core.emulator.clear_selection();
                false
            }
            2 => {
                core.emulator.start_selection(SelectionType::Semantic, point, side);
                true
            }
            _ => {
                core.emulator.start_selection(SelectionType::Lines, point, side);
                true
            }
        };
        drop(core);
        self.drag = Some(Drag { origin: event.position, armed });
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(mut drag) = self.drag.filter(|_| event.pressed_button == Some(MouseButton::Left)) else { return };
        if !drag.armed {
            let travel = event.position - drag.origin;
            if f32::from(travel.x).hypot(f32::from(travel.y)) < DRAG_THRESHOLD {
                return;
            }
            let Some((anchor, side)) = self.hit(drag.origin) else { return };
            self.session.shared.lock().emulator.start_selection(SelectionType::Simple, anchor, side);
            drag.armed = true;
            self.drag = Some(drag);
        }
        if let Some((point, side)) = self.hit(event.position) {
            self.session.shared.lock().emulator.update_selection(point, side);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag = None;
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
        let (row, col, _) = hit_cell(
            f32::from(event.position.x - geometry.origin.x),
            f32::from(event.position.y - geometry.origin.y),
            geometry.cell_width,
            geometry.line_height,
            geometry.cols,
            geometry.rows,
        );
        let input = {
            let mut core = self.session.shared.lock();
            let input = if event.modifiers.shift { None } else { core.emulator.wheel_input(lines, row, col) };
            if input.is_none() {
                core.emulator.scroll(lines);
            }
            input
        };
        cx.stop_propagation();
        match input {
            Some(input) => self.input(&input, cx),
            None => cx.notify(),
        }
    }

    fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let cursor = self.frame.borrow().cursor?;
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
            self.session.shared.lock().emulator.set_theme(theme);
            self.theme = Some(theme);
        }
        let paint = colors::paint(cx);
        let focused = self.focus.is_focused(window);
        let shared = self.session.shared.clone();
        let menu_focus = self.focus.clone();
        let element = TerminalElement::new(
            cx.entity(),
            self.session.shared.clone(),
            self.frame.clone(),
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
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
