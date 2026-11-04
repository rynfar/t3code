//! Local-only `TerminalFontPreview`: production VT emulator, grid painter and text input.

use crate::{
    terminal::{
        colors,
        element::{PADDING, RowCache, TerminalElement, TerminalSurface},
        pane::Geometry,
        session::{Frame, Front},
    },
    theme::{self, hsla, palette},
};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, EntityInputHandler, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Pixels, Point, Render, SharedString, Styled, UTF16Selection,
    Window, div, px,
};
use std::{cell::RefCell, ops::Range, rc::Rc};
use t3_core::{
    client_settings::typography::{
        FontRole,
        preview::{LocalEcho, TERMINAL_PROMPT},
    },
    terminal::{Grid, KeyInput, emulator::Emulator, encode_key},
};

fn transcript() -> String {
    format!(
        "{TERMINAL_PROMPT}vpr dev\r\n\r\n  \x1b[1;32mVITE\x1b[0m \x1b[32mv7.1.1\x1b[0m  \x1b[2mready in\x1b[0m \x1b[1m1.24s\x1b[0m\r\n\r\n  \x1b[32m→\x1b[0m  \x1b[2mLocal:\x1b[0m    \x1b[4;36mhttp://127.0.0.1:5173/\x1b[0m\r\n  \x1b[32m→\x1b[0m  \x1b[2mNetwork:\x1b[0m  \x1b[4;36mhttp://192.168.1.24:5173/\x1b[0m\r\n\r\n  \x1b[32m✓ 85 passed\x1b[0m   \x1b[33m△ 2 warnings\x1b[0m   \x1b[31m✗ 0 failed\x1b[0m\r\n\r\n  \x1b[42;30m READY \x1b[0m \x1b[2mwatching for changes, press\x1b[0m \x1b[1mq\x1b[0m \x1b[2mto quit\x1b[0m\r\n\r\n{TERMINAL_PROMPT}"
    )
}

pub(super) struct TerminalPreview {
    emulator: Emulator,
    echo: LocalEcho,
    focus: FocusHandle,
    front: Front,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    geometry: Option<Geometry>,
    marked: Option<String>,
    visible_rows: u16,
}

impl TerminalPreview {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = transcript();
        let visible_rows = transcript.split("\r\n").count() as u16;
        let mut emulator = Emulator::new(Grid::new(100, visible_rows), colors::theme_colors(cx));
        emulator.feed(transcript.as_bytes());
        let focus = cx.focus_handle();
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus("terminal-font-preview", focus.clone(), cx);
        Self {
            emulator,
            echo: LocalEcho::default(),
            focus,
            front: Front::default(),
            frame: Rc::default(),
            rows: Rc::default(),
            geometry: None,
            marked: None,
            visible_rows,
        }
    }

    fn input(&mut self, data: &str, cx: &mut Context<Self>) {
        let output = self.echo.input(data);
        if !output.is_empty() {
            self.emulator.feed(output.as_bytes());
            cx.notify();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "tab" {
            return;
        }
        let key = KeyInput {
            key: &stroke.key,
            key_char: stroke.key_char.as_deref(),
            control: stroke.modifiers.control,
            alt: stroke.modifiers.alt,
            shift: stroke.modifiers.shift,
            platform: stroke.modifiers.platform,
        };
        if let Some(bytes) = encode_key(&key, false) {
            self.input(&String::from_utf8_lossy(&bytes), cx);
        }
        if stroke.modifiers.platform
            || stroke.modifiers.control
            || matches!(
                stroke.key.as_str(),
                "enter" | "backspace" | "left" | "right" | "up" | "down"
            )
        {
            cx.stop_propagation();
        }
    }
}

impl Focusable for TerminalPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalSurface for TerminalPreview {
    fn fit(&mut self, grid: Grid, cell: (f32, f32), cx: &mut Context<Self>) {
        self.emulator.set_theme(colors::theme_colors(cx));
        self.emulator.resize(grid);
        self.emulator.set_cell_size(cell.0, cell.1);
        self.emulator
            .snapshot(&mut self.frame.borrow_mut().snapshot);
    }
    fn set_geometry(&mut self, geometry: Geometry) {
        self.geometry = Some(geometry);
    }
}

impl Render for TerminalPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = theme::font_size(FontRole::Terminal, cx);
        let id = window
            .text_system()
            .resolve_font(&gpui::font(theme::font_family(FontRole::Terminal, cx)));
        let glyph_height = window.text_system().ascent(id, px(font_size))
            + window.text_system().descent(id, px(font_size)).abs();
        let line_height = (font_size * 1.35)
            .round()
            .max(f32::from(glyph_height).ceil());
        let p = palette(cx);
        div()
            .id("terminal-font-preview")
            .track_focus(&self.focus)
            .w_full()
            .h(px(line_height * self.visible_rows as f32
                + PADDING * 2.
                + 2.))
            .rounded(theme::rem(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.terminal_background))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::key_down))
            .child(TerminalElement::new(
                cx.entity(),
                self.front.clone(),
                self.frame.clone(),
                self.rows.clone(),
                self.focus.clone(),
                self.focus.is_focused(window),
                self.marked.clone().map(SharedString::from),
            ))
    }
}

impl EntityInputHandler for TerminalPreview {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self.marked.as_ref()?.encode_utf16().collect();
        let range = range.start.min(units.len())..range.end.min(units.len());
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(&units[range]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.input(text, cx);
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
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let cursor = self.frame.borrow().snapshot.cursor?;
        Some(Bounds::new(
            geometry.origin
                + gpui::point(
                    px(cursor.col as f32 * geometry.cell_width),
                    px(cursor.row as f32 * geometry.line_height),
                ),
            gpui::size(px(geometry.cell_width), px(geometry.line_height)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
