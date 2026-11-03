//! Paints one terminal grid from the session's newest published frame, never from the emulator
//! itself. Cell backgrounds and selection merge into runs of quads; each row is shaped as a few
//! text runs pinned to the cell grid (`force_width`), breaking only at wide characters, and shaped
//! rows are cached by content so a frame reshapes only the rows that changed. Metrics follow
//! `apps/web/src/terminal/ghostty`: a line of `round(size * 1.35)`, 4px padding, and the grid
//! anchored to the bottom once there is scrollback. The font is the user's terminal typography.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, FocusHandle, Font, FontFeatures, FontStyle,
    FontWeight, Global, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, outline, point,
    px, relative, size,
};
use t3_core::terminal::emulator::{CellView, CursorStyle, Snapshot};
use t3_core::terminal::{Grid, TerminalTypography};
use t3_core::client_settings::{ClientSettingsDoc, typography::FontRole};

use super::colors::{self, to_hsla};
use super::pane::{Geometry, TerminalPane};
use super::session::{Frame, Front, take_newer};
use crate::theme::metrics;

pub const PADDING: f32 = 4.;
const SCROLLBAR_WIDTH: f32 = 6.;
const SCROLLBAR_INSET: f32 = 4.;
const MIN_THUMB: f32 = 18.;
/// The terminal typography every mounted surface paints with; panes repaint when it changes.
pub struct TerminalFont(pub TerminalTypography);

impl Global for TerminalFont {}

/// The terminal typography the client settings ask for.
pub fn typography(settings: &serde_json::Map<String, serde_json::Value>) -> TerminalTypography {
    let doc = ClientSettingsDoc::from_json(&serde_json::Value::Object(settings.clone()).to_string());
    let typography = doc.typography();
    let family = typography.family(FontRole::Terminal);
    TerminalTypography { family: (!family.is_empty()).then(|| family.to_owned()), size: typography.size(FontRole::Terminal) }
}

/// Applies new typography to every mounted terminal.
pub fn set_typography(typography: TerminalTypography, cx: &mut App) {
    if cx.try_global::<TerminalFont>().is_none_or(|current| current.0 != typography) {
        cx.set_global(TerminalFont(typography));
    }
}

fn terminal_font(cx: &App) -> (Font, f32) {
    let size = cx.try_global::<TerminalFont>().map_or(metrics::DEFAULT_CODE_FONT_SIZE, |TerminalFont(typography)| typography.size);
    let family = crate::theme::font_family(FontRole::Terminal, cx);
    let mut font = gpui::font(family);
    font.features = FontFeatures(std::sync::Arc::new(vec![("liga".into(), 0), ("calt".into(), 0), ("dlig".into(), 0)]));
    (font, size)
}

/// A shaped row: its segments and the grid column each starts at.
type ShapedRow = Rc<[(usize, ShapedLine)]>;

/// Shaped rows by content, kept for the rows the last frame painted.
#[derive(Default)]
pub struct RowCache {
    current: HashMap<u64, ShapedRow>,
    previous: HashMap<u64, ShapedRow>,
}

impl RowCache {
    fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    fn row(&mut self, key: u64, shape: impl FnOnce() -> Vec<(usize, ShapedLine)>) -> ShapedRow {
        if let Some(hit) = self.current.get(&key) {
            return hit.clone();
        }
        let row: ShapedRow = self.previous.remove(&key).unwrap_or_else(|| shape().into());
        self.current.insert(key, row.clone());
        row
    }
}

pub struct TerminalElement {
    pane: Entity<TerminalPane>,
    front: Front,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    focus: FocusHandle,
    focused: bool,
    marked: Option<SharedString>,
}

impl TerminalElement {
    pub fn new(
        pane: Entity<TerminalPane>,
        front: Front,
        frame: Rc<RefCell<Frame>>,
        rows: Rc<RefCell<RowCache>>,
        focus: FocusHandle,
        focused: bool,
        marked: Option<SharedString>,
    ) -> Self {
        TerminalElement { pane, front, frame, rows, focus, focused, marked }
    }
}

pub struct Prepainted {
    quads: Vec<PaintQuad>,
    cursor_block: Option<PaintQuad>,
    lines: Vec<(Pixels, ShapedRow)>,
    origin_x: Pixels,
    cell_width: Pixels,
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
        let (font, size_px) = terminal_font(cx);
        let font_size = px(size_px);
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system.em_advance(font_id, font_size).unwrap_or(px(size_px * 0.6));
        let glyph_height = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size).abs();
        let line_height = px((size_px * 1.35).round().max(f32::from(glyph_height).ceil()));
        let inner = size(bounds.size.width - px(2. * PADDING), bounds.size.height - px(2. * PADDING));
        let measured = Grid::new(
            (f32::from(inner.width) / f32::from(cell_width)).floor().max(1.) as u16,
            (f32::from(inner.height) / f32::from(line_height)).floor().max(1.) as u16,
        );
        self.pane.update(cx, |pane, cx| pane.fit(measured, (f32::from(cell_width), f32::from(line_height)), cx));
        let mut frame = self.frame.borrow_mut();
        take_newer(&self.front, &mut frame);
        let frame = &frame.snapshot;
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

        let style_key = {
            let mut hasher = DefaultHasher::new();
            font.family.hash(&mut hasher);
            size_px.to_bits().hash(&mut hasher);
            f32::from(cell_width).to_bits().hash(&mut hasher);
            hash_color(paint.background, &mut hasher);
            hasher.finish()
        };
        let mut rows = self.rows.borrow_mut();
        rows.begin_frame();
        let mut lines = Vec::with_capacity(frame.rows);
        for row in 0..frame.rows {
            let glyph_col = cursor_glyph.filter(|(r, _)| *r == row).map(|(_, col)| col);
            let key = row_key(frame, row, glyph_col, style_key);
            let shaped = rows.row(key, || {
                let mut shaped = Vec::new();
                shape_row(frame, row, &font, font_size, cell_width, glyph_col, paint.background, window, |col, line| {
                    shaped.push((col, line))
                });
                shaped
            });
            if !shaped.is_empty() {
                lines.push((origin.y + line_height * row as f32, shaped));
            }
        }
        drop(rows);

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

        self.pane.update(cx, |pane, _| pane.set_geometry(geometry));
        Prepainted { quads, cursor_block, lines, origin_x: origin.x, cell_width, overlays, marked, line_height }
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
            for (y, segments) in &prepainted.lines {
                for (col, line) in segments.iter() {
                    let origin = point(prepainted.origin_x + prepainted.cell_width * *col as f32, *y);
                    let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
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

fn hash_color(color: Hsla, hasher: &mut impl Hasher) {
    for channel in [color.h, color.s, color.l, color.a] {
        channel.to_bits().hash(hasher);
    }
}

/// Everything `shape_row` reads for one row; equal keys shape to the same segments.
fn row_key(frame: &Snapshot, row: usize, cursor_col: Option<usize>, style_key: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    style_key.hash(&mut hasher);
    cursor_col.hash(&mut hasher);
    let start = row * frame.cols;
    for (offset, cell) in frame.row(row).iter().enumerate() {
        let flags = [cell.bold, cell.italic, cell.dim, cell.underline, cell.strikeout, cell.wide, cell.spacer];
        (cell.c, cell.fg.r, cell.fg.g, cell.fg.b, flags).hash(&mut hasher);
        if cell.combining {
            frame.combining(start + offset).for_each(|c| c.hash(&mut hasher));
        }
    }
    hasher.finish()
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
/// Combining marks join their base character's cluster.
#[allow(clippy::too_many_arguments)]
fn shape_row(
    frame: &Snapshot,
    row: usize,
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    cursor_col: Option<usize>,
    cursor_text: Hsla,
    window: &Window,
    mut emit: impl FnMut(usize, ShapedLine),
) {
    let cells = frame.row(row);
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
        let before = text.len();
        text.push(if cell.c == '\0' { ' ' } else { cell.c });
        if cell.combining {
            text.extend(frame.combining(row * frame.cols + col));
        }
        let len = text.len() - before;
        match runs.last_mut() {
            Some(last)
                if last.color == color && last.font == run_font && last.underline == underline && last.strikethrough == strikethrough =>
            {
                last.len += len;
            }
            _ => runs.push(TextRun { len, font: run_font, color, background_color: None, underline, strikethrough }),
        }
        if cell.wide {
            flush(&mut text, &mut runs, start);
        }
    }
    flush(&mut text, &mut runs, start);
}
