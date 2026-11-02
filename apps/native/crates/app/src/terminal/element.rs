//! Paints one terminal grid. Each frame copies the visible rows out of the emulator under its
//! lock, merges cell backgrounds and selection into runs of quads, and shapes each row as a few
//! text runs pinned to the cell grid (`force_width`), breaking only at wide characters. Metrics
//! follow `apps/web/src/terminal/ghostty`: 12px mono, a line of `round(size * 1.35)`, 4px padding,
//! and the grid anchored to the bottom once there is scrollback.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, FocusHandle, Font, FontFeatures, FontStyle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, outline, point, px, relative,
    size,
};
use gpui_component::ActiveTheme;
use t3_core::terminal::Grid;
use t3_core::terminal::emulator::{CellView, CursorStyle, Snapshot};

use super::colors::{self, to_hsla};
use super::pane::{Geometry, TerminalPane};
use super::session::Shared;

pub const FONT_SIZE: f32 = 12.;
pub const PADDING: f32 = 4.;
const SCROLLBAR_WIDTH: f32 = 6.;
const SCROLLBAR_INSET: f32 = 4.;
const MIN_THUMB: f32 = 18.;

pub fn terminal_font(cx: &App) -> Font {
    let mut font = gpui::font(cx.theme().mono_font_family.clone());
    font.features = FontFeatures(std::sync::Arc::new(vec![("liga".into(), 0), ("calt".into(), 0), ("dlig".into(), 0)]));
    font
}

pub struct TerminalElement {
    pane: Entity<TerminalPane>,
    shared: Shared,
    frame: Rc<RefCell<Snapshot>>,
    focus: FocusHandle,
    focused: bool,
    marked: Option<SharedString>,
}

impl TerminalElement {
    pub fn new(
        pane: Entity<TerminalPane>,
        shared: Shared,
        frame: Rc<RefCell<Snapshot>>,
        focus: FocusHandle,
        focused: bool,
        marked: Option<SharedString>,
    ) -> Self {
        TerminalElement { pane, shared, frame, focus, focused, marked }
    }
}

pub struct Prepainted {
    quads: Vec<PaintQuad>,
    cursor_block: Option<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    overlays: Vec<PaintQuad>,
    marked: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepainted {
        let font = terminal_font(cx);
        let font_size = px(FONT_SIZE);
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system.em_advance(font_id, font_size).unwrap_or(px(FONT_SIZE * 0.6));
        let glyph_height = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size).abs();
        let line_height = px((FONT_SIZE * 1.35).round().max(f32::from(glyph_height).ceil()));
        let inner = size(bounds.size.width - px(2. * PADDING), bounds.size.height - px(2. * PADDING));
        let measured = Grid::new(
            (f32::from(inner.width) / f32::from(cell_width)).floor().max(1.) as u16,
            (f32::from(inner.height) / f32::from(line_height)).floor().max(1.) as u16,
        );
        let mut frame = self.frame.borrow_mut();
        {
            let mut core = self.shared.lock();
            let local = Grid::fit_within(core.buffer.grid, measured);
            core.emulator.resize(local);
            core.emulator.set_cell_size(f32::from(cell_width), f32::from(line_height));
            core.emulator.snapshot(&mut frame);
        }
        let slack = if frame.history > 0 {
            (f32::from(inner.height) - frame.rows as f32 * f32::from(line_height)).max(0.)
        } else {
            0.
        };
        let origin = point(bounds.left() + px(PADDING), bounds.top() + px(PADDING + slack));
        let geometry = Geometry {
            origin,
            cell_width: f32::from(cell_width),
            line_height: f32::from(line_height),
            cols: frame.cols,
            rows: frame.rows,
        };

        let paint = colors::paint(cx);
        let cell = |row: usize, col: usize, cols: usize| {
            Bounds::new(
                point(origin.x + cell_width * col as f32, origin.y + line_height * row as f32),
                size(cell_width * cols as f32, line_height),
            )
        };
        let mut quads = Vec::new();
        let mut selections = Vec::new();
        for row in 0..frame.rows {
            let cells = frame.row(row);
            push_runs(cells, |c| c.bg.map(to_hsla), |start, len, color| quads.push(fill(cell(row, start, len), color)));
            push_runs(cells, |c| c.selected.then_some(paint.selection), |start, len, color| {
                selections.push(fill(cell(row, start, len), color))
            });
        }
        quads.append(&mut selections);

        let cursor = frame.cursor;
        let block = cursor.filter(|c| self.focused && c.style == CursorStyle::Block);
        let mut overlays = Vec::new();
        let cursor_block = block.map(|c| fill(cell(c.row, c.col, if c.wide { 2 } else { 1 }), paint.cursor));
        if let Some(c) = cursor.filter(|_| block.is_none()) {
            let bounds = cell(c.row, c.col, if c.wide { 2 } else { 1 });
            overlays.push(match c.style {
                CursorStyle::Beam => fill(Bounds::new(bounds.origin, size(px(2.), line_height)), paint.cursor),
                CursorStyle::Underline => fill(
                    Bounds::new(point(bounds.left(), bounds.bottom() - px(2.)), size(bounds.size.width, px(2.))),
                    paint.cursor,
                ),
                CursorStyle::Block | CursorStyle::Hollow => outline(bounds, paint.cursor, gpui::BorderStyle::Solid),
            });
        }
        let cursor_glyph = block.map(|c| (c.row, c.col));

        let mut lines = Vec::new();
        for row in 0..frame.rows {
            let y = origin.y + line_height * row as f32;
            let glyph_col = cursor_glyph.filter(|(r, _)| *r == row).map(|(_, col)| col);
            shape_row(frame.row(row), &font, font_size, cell_width, glyph_col, paint.background, window, |col, line| {
                lines.push((point(origin.x + cell_width * col as f32, y), line))
            });
        }

        if frame.display_offset > 0 {
            let total = (frame.history + frame.rows) as f32;
            let track = f32::from(bounds.size.height) - 2. * PADDING;
            let thumb = (track * frame.rows as f32 / total).max(MIN_THUMB).min(track);
            let top = (track - thumb) * (1. - frame.display_offset as f32 / frame.history.max(1) as f32);
            let x = bounds.right() - px(SCROLLBAR_INSET + SCROLLBAR_WIDTH);
            overlays.push(
                fill(Bounds::new(point(x, bounds.top() + px(PADDING + top)), size(px(SCROLLBAR_WIDTH), px(thumb))), paint.scrollbar)
                    .corner_radii(px(SCROLLBAR_WIDTH / 2.)),
            );
        }

        let marked = self.marked.clone().zip(frame.cursor).map(|(text, c)| {
            let foreground = to_hsla(colors::theme_colors(cx).foreground);
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: foreground,
                background_color: Some(paint.background),
                underline: Some(UnderlineStyle { thickness: px(1.), color: Some(foreground), wavy: false }),
                strikethrough: None,
            };
            let line = window.text_system().shape_line(text, font_size, &[run], None);
            (cell(c.row, c.col, 1).origin, line)
        });

        drop(frame);
        self.pane.update(cx, |pane, cx| pane.laid_out(measured, geometry, cx));
        Prepainted { quads, cursor_block, lines, overlays, marked, line_height }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepainted: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(&self.focus, ElementInputHandler::new(bounds, self.pane.clone()), cx);
        let line_height = prepainted.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepainted.quads.drain(..) {
                window.paint_quad(quad);
            }
            if let Some(cursor) = prepainted.cursor_block.take() {
                window.paint_quad(cursor);
            }
            for (origin, line) in &prepainted.lines {
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }
            for quad in prepainted.overlays.drain(..) {
                window.paint_quad(quad);
            }
            if let Some((origin, line)) = &prepainted.marked {
                let _ = line.paint_background(*origin, line_height, TextAlign::Left, None, window, cx);
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }
        });
    }
}

/// Calls `emit(start, len, color)` for each run of adjacent cells with the same color.
fn push_runs(cells: &[CellView], color_of: impl Fn(&CellView) -> Option<Hsla>, mut emit: impl FnMut(usize, usize, Hsla)) {
    let mut run: Option<(usize, Hsla)> = None;
    for (col, cell) in cells.iter().enumerate() {
        let color = color_of(cell);
        if let Some((start, current)) = run
            && Some(current) != color
        {
            emit(start, col - start, current);
            run = None;
        }
        if run.is_none() {
            run = color.map(|color| (col, color));
        }
    }
    if let Some((start, color)) = run {
        emit(start, cells.len() - start, color);
    }
}

/// Shapes one row into segments that start at a grid column. Glyphs inside a segment are forced
/// onto the cell grid; a wide character gets its own segment so the cells after it stay aligned.
#[allow(clippy::too_many_arguments)]
fn shape_row(
    cells: &[CellView],
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    cursor_col: Option<usize>,
    cursor_text: Hsla,
    window: &Window,
    mut emit: impl FnMut(usize, ShapedLine),
) {
    let last_text = cells.iter().rposition(|cell| cell.c != ' ' || cell.underline || cell.strikeout).map_or(0, |last| last + 1);
    let end = cursor_col.map_or(last_text, |col| last_text.max(col + 1)).min(cells.len());
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut start = 0;
    let mut flush = |text: &mut String, runs: &mut Vec<TextRun>, start: usize| {
        if !text.is_empty() {
            let shaped = window.text_system().shape_line(SharedString::from(std::mem::take(text)), font_size, runs, Some(cell_width));
            runs.clear();
            emit(start, shaped);
        }
    };
    for (col, cell) in cells[..end].iter().enumerate() {
        if cell.spacer {
            continue;
        }
        if cell.wide {
            flush(&mut text, &mut runs, start);
        }
        if text.is_empty() {
            start = col;
        }
        let mut color = if cursor_col == Some(col) { cursor_text } else { to_hsla(cell.fg) };
        if cell.dim {
            color.a *= 0.5;
        }
        let run_font = Font {
            weight: if cell.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
            style: if cell.italic { FontStyle::Italic } else { FontStyle::Normal },
            ..font.clone()
        };
        let underline = cell.underline.then_some(UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false });
        let strikethrough = cell.strikeout.then_some(StrikethroughStyle { thickness: px(1.), color: Some(color) });
        let c = if cell.c == '\0' { ' ' } else { cell.c };
        text.push(c);
        match runs.last_mut() {
            Some(last)
                if last.color == color && last.font == run_font && last.underline == underline && last.strikethrough == strikethrough =>
            {
                last.len += c.len_utf8();
            }
            _ => runs.push(TextRun { len: c.len_utf8(), font: run_font, color, background_color: None, underline, strikethrough }),
        }
        if cell.wide {
            flush(&mut text, &mut runs, start);
        }
    }
    flush(&mut text, &mut runs, start);
}
