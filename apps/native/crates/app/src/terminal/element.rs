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
    App, Bounds, Context, ElementInputHandler, ContentMask, EntityInputHandler, Element, ElementId, Entity, FocusHandle, Font, FontFeatures, FontStyle,
    FontWeight, Global, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, PathBuilder, Pixels, Point,
    ShapedLine, SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, outline, point,
    px, relative, size,
};
use t3_core::terminal::emulator::{CellView, CursorStyle, Snapshot};
use t3_core::terminal::links::LinkHit;
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

pub(crate) trait TerminalSurface: EntityInputHandler {
    fn fit(&mut self, grid: Grid, cell: (f32, f32), cx: &mut Context<Self>);
    fn set_geometry(&mut self, geometry: Geometry);
}

impl TerminalSurface for TerminalPane {
    fn fit(&mut self, grid: Grid, cell: (f32, f32), cx: &mut Context<Self>) { self.fit(grid, cell, cx); }
    fn set_geometry(&mut self, geometry: Geometry) { self.set_geometry(geometry); }
}

pub(crate) struct TerminalElement<T: TerminalSurface> {
    pane: Entity<T>,
    front: Front,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    focus: FocusHandle,
    focused: bool,
    marked: Option<SharedString>,
    hovered_link: Option<LinkHit>,
}

impl<T: TerminalSurface> TerminalElement<T> {
    pub fn new(
        pane: Entity<T>,
        front: Front,
        frame: Rc<RefCell<Frame>>,
        rows: Rc<RefCell<RowCache>>,
        focus: FocusHandle,
        focused: bool,
        marked: Option<SharedString>,
    ) -> Self {
        TerminalElement { pane, front, frame, rows, focus, focused, marked, hovered_link: None }
    }

    pub fn hovered_link(mut self, link: Option<LinkHit>) -> Self {
        self.hovered_link = link;
        self
    }
}

pub struct Prepainted {
    quads: Vec<PaintQuad>,
    cursor_block: Option<PaintQuad>,
    powerline: Vec<PowerlineCell>,
    lines: Vec<(Pixels, ShapedRow)>,
    origin_x: Pixels,
    cell_width: Pixels,
    overlays: Vec<PaintQuad>,
    marked: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PowerlineShape {
    Fill(&'static [(f32, f32)]),
    Stroke(&'static [(f32, f32)]),
    Cap { flat_left: bool, filled: bool },
}

#[derive(Clone, Copy)]
struct PowerlineCell {
    bounds: Bounds<Pixels>,
    shape: PowerlineShape,
    color: Hsla,
}

/// Maps Powerline private-use glyphs (`U+E0B0..=U+E0BF`) to exact cell-edge geometry so dividers,
/// triangles, slants, and rounded caps never show font-inset gaps (`pingdotgg/t3code#24888`).
fn powerline_shape(ch: char) -> Option<PowerlineShape> {
    Some(match ch {
        '\u{e0b0}' => PowerlineShape::Fill(&[(0.0, 0.0), (1.0, 0.5), (0.0, 1.0)]),
        '\u{e0b1}' => PowerlineShape::Stroke(&[(0.0, 0.0), (1.0, 0.5), (0.0, 1.0)]),
        '\u{e0b2}' => PowerlineShape::Fill(&[(1.0, 0.0), (0.0, 0.5), (1.0, 1.0)]),
        '\u{e0b3}' => PowerlineShape::Stroke(&[(1.0, 0.0), (0.0, 0.5), (1.0, 1.0)]),
        '\u{e0b4}' => PowerlineShape::Cap { flat_left: true, filled: true },
        '\u{e0b5}' => PowerlineShape::Cap { flat_left: true, filled: false },
        '\u{e0b6}' => PowerlineShape::Cap { flat_left: false, filled: true },
        '\u{e0b7}' => PowerlineShape::Cap { flat_left: false, filled: false },
        '\u{e0b8}' => PowerlineShape::Fill(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        '\u{e0b9}' => PowerlineShape::Stroke(&[(0.0, 0.0), (1.0, 1.0)]),
        '\u{e0ba}' => PowerlineShape::Fill(&[(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        '\u{e0bb}' => PowerlineShape::Stroke(&[(1.0, 0.0), (0.0, 1.0)]),
        '\u{e0bc}' => PowerlineShape::Fill(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]),
        '\u{e0bd}' => PowerlineShape::Stroke(&[(1.0, 0.0), (0.0, 1.0)]),
        '\u{e0be}' => PowerlineShape::Fill(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]),
        '\u{e0bf}' => PowerlineShape::Stroke(&[(0.0, 0.0), (1.0, 1.0)]),
        _ => return None,
    })
}

fn paint_powerline_cell(cell: PowerlineCell, clip_bounds: Bounds<Pixels>, window: &mut Window) {
    let left = f32::from(cell.bounds.left());
    let top = f32::from(cell.bounds.top());
    let width = f32::from(cell.bounds.size.width);
    let height = f32::from(cell.bounds.size.height);
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    match cell.shape {
        PowerlineShape::Fill(points) => {
            if let Some((&first, rest)) = points.split_first() {
                let mut builder = PathBuilder::fill();
                builder.move_to(point(px(left + first.0 * width), px(top + first.1 * height)));
                for &(x, y) in rest {
                    builder.line_to(point(px(left + x * width), px(top + y * height)));
                }
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, cell.color);
                }
            }
        }
        PowerlineShape::Stroke(points) => {
            if let Some((&first, rest)) = points.split_first() {
                let mut builder = PathBuilder::stroke(px(1.));
                builder.move_to(point(px(left + first.0 * width), px(top + first.1 * height)));
                for &(x, y) in rest {
                    builder.line_to(point(px(left + x * width), px(top + y * height)));
                }
                if let Ok(path) = builder.build() {
                    let cell_clip = cell.bounds.intersect(&clip_bounds);
                    window.with_content_mask(Some(ContentMask { bounds: cell_clip }), |window| {
                        window.paint_path(path, cell.color);
                    });
                }
            }
        }
        PowerlineShape::Cap { flat_left, filled } => {
            const STEPS: usize = 16;
            let inset = if filled { 0.0 } else { 0.5 };
            let rx = (width - inset).max(0.5);
            let ry = (height / 2.0 - inset).max(0.5);
            let cy = top + height / 2.0;
            let base_x = if flat_left { left } else { left + width };
            let start_angle = if flat_left { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 };
            let arc_point = |step: usize| {
                let theta = start_angle + std::f32::consts::PI * (step as f32 / STEPS as f32);
                point(px(base_x + rx * theta.cos()), px(cy + ry * theta.sin()))
            };
            if filled {
                let mut builder = PathBuilder::fill();
                builder.move_to(arc_point(0));
                for step in 1..=STEPS {
                    builder.line_to(arc_point(step));
                }
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, cell.color);
                }
                // Overlap the neighbor segment boundary so fractional cell widths avoid a hairline seam.
                let seam_x = if flat_left { left - 0.5 } else { left + width };
                window.paint_quad(fill(
                    Bounds::new(point(px(seam_x), px(top)), size(px(0.5), px(height))),
                    cell.color,
                ));
            } else {
                let mut builder = PathBuilder::stroke(px(1.));
                builder.move_to(arc_point(0));
                for step in 1..=STEPS {
                    builder.line_to(arc_point(step));
                }
                if let Ok(path) = builder.build() {
                    let cell_clip = cell.bounds.intersect(&clip_bounds);
                    window.with_content_mask(Some(ContentMask { bounds: cell_clip }), |window| {
                        window.paint_path(path, cell.color);
                    });
                }
            }
        }
    }
}

impl<T: TerminalSurface> IntoElement for TerminalElement<T> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<T: TerminalSurface> Element for TerminalElement<T> {
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
        let pane_bg = if frame.cols > 0 { to_hsla(frame.background) } else { paint.background };
        let cursor_color = if frame.cols > 0 { to_hsla(frame.cursor_color) } else { paint.cursor };
        let cell = |row: usize, col: usize, cols: usize| {
            Bounds::new(
                point(origin.x + cell_width * col as f32, origin.y + line_height * row as f32),
                size(cell_width * cols as f32, line_height),
            )
        };
        // Paint the full pane background (including padding) in the emulator's active default
        // background so OSC 11 changes repaint the whole surface (`pingdotgg/t3code#37193`).
        let mut quads = vec![fill(bounds, pane_bg)];
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
        let cursor_block = block.map(|c| fill(cell(c.row, c.col, if c.wide { 2 } else { 1 }), cursor_color));
        if let Some(c) = cursor.filter(|_| block.is_none()) {
            let bounds = cell(c.row, c.col, if c.wide { 2 } else { 1 });
            overlays.push(match c.style {
                CursorStyle::Beam => fill(Bounds::new(bounds.origin, size(px(2.), line_height)), cursor_color),
                CursorStyle::Underline => fill(
                    Bounds::new(point(bounds.left(), bounds.bottom() - px(2.)), size(bounds.size.width, px(2.))),
                    cursor_color,
                ),
                CursorStyle::Block | CursorStyle::Hollow => outline(bounds, cursor_color, gpui::BorderStyle::Solid),
            });
        }
        let cursor_glyph = block.map(|c| (c.row, c.col));

        let mut powerline = Vec::new();
        for row in 0..frame.rows {
            let cells = frame.row(row);
            for (col, cell_view) in cells.iter().enumerate() {
                if cell_view.spacer || cell_view.hidden || cell_view.combining {
                    continue;
                }
                if let Some(shape) = powerline_shape(cell_view.c) {
                    let mut color = if cursor_glyph == Some((row, col)) { pane_bg } else { to_hsla(cell_view.fg) };
                    if cell_view.dim {
                        color.a *= 0.5;
                    }
                    powerline.push(PowerlineCell {
                        bounds: cell(row, col, if cell_view.wide { 2 } else { 1 }),
                        shape,
                        color,
                    });
                }
            }
        }

        let style_key = {
            let mut hasher = DefaultHasher::new();
            font.family.hash(&mut hasher);
            size_px.to_bits().hash(&mut hasher);
            f32::from(cell_width).to_bits().hash(&mut hasher);
            hash_color(pane_bg, &mut hasher);
            hasher.finish()
        };
        let mut rows = self.rows.borrow_mut();
        rows.begin_frame();
        let mut lines = Vec::with_capacity(frame.rows);
        for row in 0..frame.rows {
            let glyph_col = cursor_glyph.filter(|(r, _)| *r == row).map(|(_, col)| col);
            let hovered_cols = hovered_cols_for_row(self.hovered_link.as_ref(), row, frame.cols);
            let key = row_key(frame, row, glyph_col, hovered_cols, style_key);
            let shaped = rows.row(key, || {
                let mut shaped = Vec::new();
                shape_row(frame, row, &font, font_size, cell_width, glyph_col, hovered_cols, pane_bg, window, |col, line| {
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
        Prepainted { quads, cursor_block, powerline, lines, origin_x: origin.x, cell_width, overlays, marked, line_height }
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
            for cell in prepainted.powerline.drain(..) {
                paint_powerline_cell(cell, bounds, window);
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

fn hovered_cols_for_row(link: Option<&LinkHit>, row: usize, cols: usize) -> Option<(usize, usize)> {
    let link = link?;
    if row < link.start.0 || row > link.end.0 || cols == 0 {
        return None;
    }
    let start = if row == link.start.0 { link.start.1 } else { 0 };
    let end = if row == link.end.0 { link.end.1 } else { cols - 1 };
    Some((start, end))
}

/// Everything `shape_row` reads for one row; equal keys shape to the same segments.
fn row_key(frame: &Snapshot, row: usize, cursor_col: Option<usize>, hovered_cols: Option<(usize, usize)>, style_key: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    style_key.hash(&mut hasher);
    cursor_col.hash(&mut hasher);
    hovered_cols.hash(&mut hasher);
    let start = row * frame.cols;
    for (offset, cell) in frame.row(row).iter().enumerate() {
        let flags = [cell.bold, cell.italic, cell.dim, cell.hidden, cell.underline, cell.strikeout, cell.wide, cell.spacer];
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
    hovered_cols: Option<(usize, usize)>,
    cursor_text: Hsla,
    window: &Window,
    mut emit: impl FnMut(usize, ShapedLine),
) {
    let cells = frame.row(row);
    let last_text = cells.iter().rposition(|cell| cell.c != ' ' || cell.underline || cell.strikeout).map_or(0, |last| last + 1);
    let end = cursor_col
        .map_or(last_text, |col| last_text.max(col + 1))
        .max(hovered_cols.map_or(0, |(_, end)| end + 1))
        .min(cells.len());
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
        // Concealed cells keep text and decorations hidden even under the block cursor (`#37141`);
        // visible decorations stay drawn in `cursor_text` above the block cursor (`#37206`).
        let underline = (!cell.hidden && (cell.underline || hovered_cols.is_some_and(|(start, end)| col >= start && col <= end)))
            .then_some(UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false });
        let strikethrough = (!cell.hidden && cell.strikeout).then_some(StrikethroughStyle { thickness: px(1.), color: Some(color) });
        let is_powerline = !cell.hidden && !cell.combining && powerline_shape(cell.c).is_some();
        if is_powerline && underline.is_none() && strikethrough.is_none() {
            flush(&mut text, &mut runs, start);
            continue;
        }
        let before = text.len();
        if is_powerline {
            text.push(' ');
        } else {
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powerline_shapes_cover_full_e0b0_to_e0bf_range() {
        for code in 0xE0B0u32..=0xE0BF {
            let ch = char::from_u32(code).unwrap();
            assert!(powerline_shape(ch).is_some(), "expected Powerline shape for U+{code:04X}");
        }
        assert!(matches!(powerline_shape('\u{e0b0}'), Some(PowerlineShape::Fill(_))));
        assert!(matches!(powerline_shape('\u{e0b1}'), Some(PowerlineShape::Stroke(_))));
        assert_eq!(powerline_shape('\u{e0b4}'), Some(PowerlineShape::Cap { flat_left: true, filled: true }));
        assert_eq!(powerline_shape('\u{e0b5}'), Some(PowerlineShape::Cap { flat_left: true, filled: false }));
        assert_eq!(powerline_shape('\u{e0b6}'), Some(PowerlineShape::Cap { flat_left: false, filled: true }));
        assert_eq!(powerline_shape('\u{e0b7}'), Some(PowerlineShape::Cap { flat_left: false, filled: false }));
        assert_eq!(powerline_shape('A'), None);
        assert_eq!(powerline_shape('─'), None);
    }
}
