//! Selectable plain text with optional highlight spans: the native stand-in for the browser
//! selection the web's transcript text gets for free (`CommandOutputPanel.tsx`). A drag selects,
//! across wrapped lines too; a double-click takes a word, a triple-click a line, and a shift-click
//! extends. `mod+c` copies, `mod+a` selects all of the focused text, the context menu offers Copy,
//! and a click anywhere else clears it. The rules live in `t3_core::text_selection`; this maps the
//! pointer to offsets and paints. The text takes its font and colour from the parent, and the
//! selection holds while text grows underneath it or a bounded tail drops its front. It redraws only
//! when the selection changes, and a drag past its scroller's edge scrolls it.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DismissEvent, Element, ElementId, Entity, FocusHandle, GlobalElementId, HighlightStyle, Hitbox,
    HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, RenderOnce, ScrollHandle, SharedString, Styled, StyledText, Subscription, TextLayout, TextStyleRefinement, Window,
    WrappedLineLayout, div, fill, point, px,
};
use gpui_component::ActiveTheme;
use gpui_component::input::{Copy, SelectAll};
use gpui_component::menu::ContextMenuExt;
use t3_core::text_selection::{Granularity, Hit, Selection};

const CONTEXT: &str = "SelectableText";
/// A drag held past the scroller's edge scrolls it this often, by how far past it is, up to
/// `AUTOSCROLL_MAX` a step.
const AUTOSCROLL_STEP: Duration = Duration::from_millis(33);
const AUTOSCROLL_MAX: f32 = 48.;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-c", Copy, Some(CONTEXT)), KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT))]);
}

pub type Highlights = Arc<[(Range<usize>, HighlightStyle)]>;

#[derive(IntoElement)]
pub struct SelectableText {
    id: ElementId,
    text: SharedString,
    highlights: Option<Highlights>,
    origin: usize,
    scroll: Option<ScrollHandle>,
}

impl SelectableText {
    /// `id` keys the selection, so it stays the same while the text is on screen.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        SelectableText { id: id.into(), text: text.into(), highlights: None, origin: 0, scroll: None }
    }

    /// Styled byte ranges, sorted and not overlapping, as `StyledText::with_highlights` takes them.
    pub fn highlights(mut self, highlights: Highlights) -> Self {
        self.highlights = Some(highlights);
        self
    }

    /// Where `text` starts in a stream that loses bytes from its front, such as a bounded tail:
    /// as the origin grows the selection moves with its text. An origin that falls back starts over.
    pub fn origin(mut self, origin: usize) -> Self {
        self.origin = origin;
        self
    }

    /// The scroller the text sits in: a drag held past its top or bottom edge scrolls it.
    pub fn autoscroll(mut self, scroll: &ScrollHandle) -> Self {
        self.scroll = Some(scroll.clone());
        self
    }
}

struct State {
    focus: FocusHandle,
    /// The text the selection's offsets index, and where it starts in its stream.
    text: SharedString,
    origin: usize,
    selection: Option<Selection>,
    dragging: bool,
    /// Held while this text's context menu is open: a click on the menu, or the one that
    /// dismisses it, must not clear the selection it copies.
    menu: Option<Subscription>,
    scroll: Option<ScrollHandle>,
    /// The last painted layout and drag position, for auto-scroll steps between pointer moves.
    layout: Option<TextLayout>,
    pointer: Point<Pixels>,
    autoscrolling: bool,
}

impl State {
    /// Takes the text being drawn; a changed text carries the selection over or drops it.
    fn sync(&mut self, text: &SharedString, origin: usize) {
        if self.text.as_ptr() == text.as_ptr() && self.text.len() == text.len() && self.origin == origin {
            return;
        }
        let dropped = origin.saturating_sub(self.origin);
        self.selection = self.selection.take().and_then(|selection| selection.remap(&self.text, text, dropped));
        self.text = text.clone();
        self.origin = origin;
    }

    fn selected(&self) -> Option<&str> {
        self.selection.as_ref().map(|selection| selection.text(&self.text)).filter(|text| !text.is_empty())
    }

    /// A drag step at `pointer`: scrolls the scroller when the pointer is past its edge, then
    /// extends the selection to what is under the pointer. Returns whether it scrolled.
    fn drag_to(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        self.pointer = pointer;
        let scrolled = self.scroll.as_ref().is_some_and(|scroll| autoscroll(scroll, pointer));
        let text = self.text.clone();
        let hit = self.layout.as_ref().map(|layout| hit_test(layout, pointer));
        let extended = hit.is_some_and(|hit| self.selection.as_mut().is_some_and(|selection| selection.extend(&text, hit)));
        if scrolled || extended {
            cx.notify();
        }
        scrolled
    }

    /// Keeps scrolling while the drag holds the pointer past the scroller's edge.
    fn keep_scrolling(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.autoscrolling, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_STEP).await;
                let more = this.update(cx, |state, cx| state.dragging && state.drag_to(state.pointer, cx));
                if !more.unwrap_or(false) {
                    break;
                }
            }
            let _ = this.update(cx, |state, _| state.autoscrolling = false);
        })
        .detach();
    }
}

/// Scrolls toward a pointer past the scroller's top or bottom edge. Returns whether it moved.
fn autoscroll(scroll: &ScrollHandle, pointer: Point<Pixels>) -> bool {
    let offset = scroll.offset();
    let Some(y) = autoscroll_offset(scroll.bounds(), offset.y, scroll.max_offset().y, pointer.y) else { return false };
    scroll.set_offset(point(offset.x, y));
    true
}

/// The offset one step takes a scroller (`offset` from 0 down to `-max`) toward a pointer past
/// its top or bottom edge, by how far past it is; `None` inside it or with no further to go.
fn autoscroll_offset(bounds: Bounds<Pixels>, offset: Pixels, max: Pixels, pointer: Pixels) -> Option<Pixels> {
    let past = if pointer < bounds.top() {
        pointer - bounds.top()
    } else if pointer > bounds.bottom() {
        pointer - bounds.bottom()
    } else {
        return None;
    };
    let step = f32::from(past).clamp(-AUTOSCROLL_MAX, AUTOSCROLL_MAX);
    let y = px((f32::from(offset) - step).clamp(-f32::from(max), 0.));
    (y != offset).then_some(y)
}

/// `highlights` with their fills lifted where `selected` runs, so the selection shows over a
/// coloured span as `::selection` does in the browser.
fn lift_fills(highlights: &[(Range<usize>, HighlightStyle)], selected: Range<usize>) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::with_capacity(highlights.len() + 2);
    for (range, style) in highlights {
        if style.background_color.is_none() || range.end <= selected.start || range.start >= selected.end {
            out.push((range.clone(), *style));
            continue;
        }
        let inner = range.start.max(selected.start)..range.end.min(selected.end);
        if range.start < inner.start {
            out.push((range.start..inner.start, *style));
        }
        out.push((inner.clone(), HighlightStyle { background_color: None, ..*style }));
        if inner.end < range.end {
            out.push((inner.end..range.end, *style));
        }
    }
    out
}

fn copy(state: &Entity<State>, cx: &mut App) {
    match state.read(cx).selected().map(str::to_owned) {
        Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        // Nothing selected here; the window's own selection (markdown) may still copy.
        None => cx.propagate(),
    }
}

impl RenderOnce for SelectableText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let initial = (self.text.clone(), self.origin);
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| State {
            focus: cx.focus_handle(),
            text: initial.0,
            origin: initial.1,
            selection: None,
            dragging: false,
            menu: None,
            scroll: None,
            layout: None,
            pointer: Point::default(),
            autoscrolling: false,
        });
        let (focus, selected) = state.update(cx, |state, _| {
            state.sync(&self.text, self.origin);
            state.scroll = self.scroll;
            (state.focus.clone(), state.selection.as_ref().map(Selection::range).filter(|range| !range.is_empty()))
        });
        let (copy_state, all_state, menu_state, menu_focus) = (state.clone(), state.clone(), state.clone(), focus.clone());
        // The context menu draws inside this div: it gets the app's font, the text keeps the caller's.
        let font_family = window.text_style().font_family;
        div()
            .id(self.id)
            .key_context(CONTEXT)
            .track_focus(&focus)
            .font_family(cx.theme().font_family.clone())
            .on_action(move |_: &Copy, _, cx| copy(&copy_state, cx))
            .on_action(move |_: &SelectAll, _, cx| {
                all_state.update(cx, |state, cx| {
                    state.selection = Some(Selection::all(&state.text));
                    cx.notify();
                })
            })
            .child(TextElement::new(state, self.text, self.highlights, selected, font_family))
            .context_menu(move |menu, _, cx| {
                let popup = cx.entity();
                let selected = menu_state.update(cx, |state, cx| {
                    state.menu = Some(cx.subscribe(&popup, |state, _, _: &DismissEvent, _| state.menu = None));
                    state.selected().is_some()
                });
                menu.action_context(menu_focus.clone()).menu_with_disabled("Copy", Box::new(Copy), !selected).menu("Select all", Box::new(SelectAll))
            })
    }
}

/// The text and its selection, painted beneath it.
struct TextElement {
    state: Entity<State>,
    text: StyledText,
    font_family: SharedString,
}

impl TextElement {
    fn new(state: Entity<State>, text: SharedString, highlights: Option<Highlights>, selected: Option<Range<usize>>, font_family: SharedString) -> Self {
        let text = StyledText::new(text);
        let text = match (highlights, selected) {
            (Some(highlights), Some(selected)) => text.with_highlights(lift_fills(&highlights, selected)),
            (Some(highlights), None) => text.with_highlights(highlights.iter().cloned()),
            (None, _) => text,
        };
        TextElement { state, text, font_family }
    }
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let style = TextStyleRefinement { font_family: Some(self.font_family.clone()), ..TextStyleRefinement::default() };
        window.with_text_style(Some(style), |window| self.text.request_layout(id, inspector_id, window, cx))
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text.prepaint(id, inspector_id, bounds, layout, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text_layout = self.text.layout().clone();
        let (range, dragging, selected) = self.state.update(cx, |state, _| {
            state.layout = Some(text_layout.clone());
            (state.selection.as_ref().map(Selection::range).filter(|range| !range.is_empty()), state.dragging, state.selection.is_some())
        });
        if let Some(range) = range {
            let color = cx.theme().selection;
            let lines = text_layout.line_layouts();
            for rect in Lines::new(&lines, &text_layout).selection_rects(range, bounds.right()) {
                window.paint_quad(fill(rect, color));
            }
        }
        self.text.paint(id, inspector_id, bounds, layout, &mut (), window, cx);
        if dragging {
            window.set_window_cursor_style(CursorStyle::IBeam);
        } else {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }

        window.on_mouse_event({
            let (state, text_layout, hitbox) = (self.state.clone(), text_layout.clone(), hitbox.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                // A truncated layout no longer indexes the text, so it is not selectable.
                if !phase.bubble() || event.button != MouseButton::Left || !hitbox.is_hovered(window) || text_layout.len() != state.read(cx).text.len() {
                    return;
                }
                // The row behind must not read the press as a click, nor the window start its own
                // selection from it.
                cx.stop_propagation();
                let hit = hit_test(&text_layout, event.position);
                let focus = state.read(cx).focus.clone();
                window.focus(&focus, cx);
                state.update(cx, |state, cx| {
                    let text = state.text.clone();
                    match state.selection.as_mut() {
                        Some(selection) if event.modifiers.shift => {
                            selection.extend(&text, hit);
                        }
                        _ => state.selection = Some(Selection::press(&text, hit, Granularity::from_clicks(event.click_count))),
                    }
                    state.dragging = true;
                    cx.notify();
                });
            }
        });
        // Capture phase, so the drag follows the pointer over anything that stops it.
        window.on_mouse_event({
            let state = self.state.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if !phase.capture() || !state.read(cx).dragging {
                    return;
                }
                state.update(cx, |state, cx| {
                    if event.pressed_button != Some(MouseButton::Left) {
                        // Released where the window could not see it.
                        state.dragging = false;
                        cx.notify();
                    } else if state.drag_to(event.position, cx) {
                        state.keep_scrolling(cx);
                    }
                });
            }
        });
        window.on_mouse_event({
            let state = self.state.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if phase.capture() && event.button == MouseButton::Left && state.read(cx).dragging {
                    state.update(cx, |state, cx| {
                        state.dragging = false;
                        cx.notify();
                    });
                }
            }
        });
        if selected {
            window.on_mouse_event({
                let (state, hitbox) = (self.state.clone(), hitbox.clone());
                move |_: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() || hitbox.is_hovered(window) || state.read(cx).menu.is_some() {
                        return;
                    }
                    state.update(cx, |state, cx| {
                        state.selection = None;
                        cx.notify();
                    });
                }
            });
        }
    }
}

fn hit_test(layout: &TextLayout, position: Point<Pixels>) -> Hit {
    let lines = layout.line_layouts();
    Lines::new(&lines, layout).hit(position)
}

/// A laid-out text's logical lines, top to bottom from `origin` and left-aligned, as
/// `TextLayout` stacks them.
struct Lines<'a> {
    lines: &'a [Arc<WrappedLineLayout>],
    origin: Point<Pixels>,
    line_height: Pixels,
}

impl<'a> Lines<'a> {
    fn new(lines: &'a [Arc<WrappedLineLayout>], layout: &TextLayout) -> Self {
        Lines { lines, origin: layout.bounds().origin, line_height: layout.line_height() }
    }

    /// Above the text is its start and below it its end; beside a row is that row's edge.
    fn hit(&self, position: Point<Pixels>) -> Hit {
        let mut top = self.origin.y;
        let mut start = 0;
        if position.y < top {
            return Hit { caret: 0, under: 0 };
        }
        for line in self.lines {
            let bottom = top + line.size(self.line_height).height;
            if position.y < bottom {
                let local = point(position.x - self.origin.x, position.y - top);
                let (Ok(caret) | Err(caret)) = line.closest_index_for_position(local, self.line_height);
                let (Ok(under) | Err(under)) = line.index_for_position(local, self.line_height);
                return Hit { caret: start + caret, under: start + under };
            }
            top = bottom;
            start += line.len() + 1;
        }
        let end = start.saturating_sub(1);
        Hit { caret: end, under: end }
    }

    /// Where the caret before `index` sits: its x and its row's top. At a soft wrap one index
    /// ends a row and starts the next; `downstream` takes the next.
    fn position(&self, index: usize, downstream: bool) -> Option<Point<Pixels>> {
        let mut top = self.origin.y;
        let mut start = 0;
        for line in self.lines {
            let end = start + line.len();
            if index <= end {
                let local = index.checked_sub(start)?;
                let layout = &line.unwrapped_layout;
                let boundary = |row: usize| line.wrap_boundaries.get(row).map(|wrap| layout.runs[wrap.run_ix].glyphs[wrap.glyph_ix].index);
                let row = (0..line.wrap_boundaries.len()).take_while(|&row| boundary(row).is_some_and(|at| at < local || (downstream && at == local))).count();
                let row_start = row.checked_sub(1).and_then(boundary).unwrap_or(0);
                let x = layout.x_for_index(local) - layout.x_for_index(row_start);
                return Some(point(self.origin.x + x, top + self.line_height * row as f32));
            }
            top += line.size(self.line_height).height;
            start = end + 1;
        }
        None
    }

    /// The fills that mark `range`, as editors and the markdown viewer draw them: from its start to
    /// the right edge, whole rows between, and from the left edge to its end.
    fn selection_rects(&self, range: Range<usize>, right: Pixels) -> Vec<Bounds<Pixels>> {
        let (Some(start), Some(end)) = (self.position(range.start, true), self.position(range.end, false)) else { return Vec::new() };
        let height = self.line_height;
        let rects = if start.y == end.y {
            vec![Bounds::from_corners(start, point(end.x, end.y + height))]
        } else {
            vec![
                Bounds::from_corners(start, point(right, start.y + height)),
                Bounds::from_corners(point(self.origin.x, start.y + height), point(right, end.y)),
                Bounds::from_corners(point(self.origin.x, end.y), point(end.x, end.y + height)),
            ]
        };
        rects.into_iter().filter(|rect| rect.size.width > px(0.) && rect.size.height > px(0.)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontId, GlyphId, LineLayout, ShapedGlyph, ShapedRun, WrapBoundary, size};

    const LINE_HEIGHT: f32 = 20.;
    const ORIGIN: (f32, f32) = (100., 50.);

    /// A monospace line, 10px per character, whose rows after the first start at `rows_at`.
    fn line(text: &str, rows_at: &[usize]) -> Arc<WrappedLineLayout> {
        let glyphs = text.char_indices().map(|(index, _)| ShapedGlyph { id: GlyphId(0), position: point(px(10. * index as f32), px(0.)), index, is_emoji: false }).collect();
        let unwrapped = LineLayout {
            font_size: px(10.),
            width: px(10. * text.len() as f32),
            ascent: px(8.),
            descent: px(2.),
            runs: vec![ShapedRun { font_id: FontId(0), glyphs }],
            len: text.len(),
        };
        Arc::new(WrappedLineLayout {
            unwrapped_layout: Arc::new(unwrapped),
            wrap_boundaries: rows_at.iter().map(|&glyph_ix| WrapBoundary { run_ix: 0, glyph_ix }).collect(),
            wrap_width: None,
        })
    }

    fn lines(lines: &[Arc<WrappedLineLayout>]) -> Lines<'_> {
        Lines { lines, origin: point(px(ORIGIN.0), px(ORIGIN.1)), line_height: px(LINE_HEIGHT) }
    }

    /// A point in the text's own coordinates.
    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(ORIGIN.0 + x), px(ORIGIN.1 + y))
    }

    fn rect(x: f32, y: f32, width: f32) -> Bounds<Pixels> {
        Bounds::new(at(x, y), size(px(width), px(LINE_HEIGHT)))
    }

    const RIGHT: f32 = 300.;

    #[test]
    fn a_point_hits_the_nearest_boundary_and_the_character_under_it() {
        let text = [line("hello", &[]), line("world", &[])];
        let text = lines(&text);
        let hit = |x, y| {
            let hit = text.hit(at(x, y));
            (hit.caret, hit.under)
        };
        // The left part of `e` is nearest the boundary before it; the right part, the one after.
        assert_eq!(hit(14., 5.), (1, 1));
        assert_eq!(hit(16., 5.), (2, 1));
        // Past a row's end, its end; before a row's start, its start. The newline counts.
        assert_eq!(hit(150., 5.), (5, 5));
        assert_eq!(hit(-40., 25.), (6, 6));
        assert_eq!(hit(24., 25.), (8, 8));
        // Above the text is its start and below it its end.
        assert_eq!(hit(30., -10.), (0, 0));
        assert_eq!(hit(30., 90.), (11, 11));
    }

    #[test]
    fn a_point_on_a_wrapped_row_hits_that_row() {
        let text = [line("abcdefgh", &[4])];
        let text = lines(&text);
        let hit = |x, y| {
            let hit = text.hit(at(x, y));
            (hit.caret, hit.under)
        };
        assert_eq!(hit(3., 25.), (4, 4), "the second row starts at `e`");
        assert_eq!(hit(8., 25.), (5, 4));
        assert_eq!(hit(27., 25.), (7, 6));
        assert_eq!(hit(250., 25.), (8, 8));
        assert_eq!(hit(250., 5.), (4, 4), "past the first row's end is the wrap");
    }

    #[test]
    fn a_selection_on_one_row_is_one_fill() {
        let text = [line("hello", &[])];
        assert_eq!(lines(&text).selection_rects(1..3, px(ORIGIN.0 + RIGHT)), vec![rect(10., 0., 20.)]);
    }

    #[test]
    fn a_selection_across_wrapped_rows_fills_to_the_edges() {
        let text = [line("abcdefghijkl", &[4, 8])];
        let text = lines(&text);
        let right = px(ORIGIN.0 + RIGHT);
        assert_eq!(text.selection_rects(2..10, right), vec![rect(20., 0., RIGHT - 20.), rect(0., 20., RIGHT), rect(0., 40., 20.)]);
        // Starting where a row wraps starts on the next row, not as a sliver after the last one.
        assert_eq!(text.selection_rects(4..6, right), vec![rect(0., 20., 20.)]);
        // Ending where a row wraps ends on that row.
        assert_eq!(text.selection_rects(1..4, right), vec![rect(10., 0., 30.)]);
    }

    #[test]
    fn a_selection_across_lines_marks_the_newline_it_takes() {
        let text = [line("hello", &[]), line("world", &[])];
        let text = lines(&text);
        let right = px(ORIGIN.0 + RIGHT);
        assert_eq!(text.selection_rects(3..8, right), vec![rect(30., 0., RIGHT - 30.), rect(0., 20., 20.)]);
        assert_eq!(text.selection_rects(5..6, right), vec![rect(50., 0., RIGHT - 50.)], "only the newline");
    }

    #[test]
    fn a_selection_lifts_span_fills_only_where_it_runs() {
        let fill = |color: u32| HighlightStyle { background_color: Some(gpui::rgb(color).into()), ..HighlightStyle::default() };
        let bold = HighlightStyle { font_weight: Some(gpui::FontWeight::BOLD), ..HighlightStyle::default() };
        let lifted = HighlightStyle { background_color: None, ..fill(0xff0000) };
        let highlights = [(0..5, fill(0xff0000)), (5..8, bold), (8..12, fill(0x0000ff))];
        assert_eq!(
            lift_fills(&highlights, 3..10),
            vec![(0..3, fill(0xff0000)), (3..5, lifted), (5..8, bold), (8..10, HighlightStyle { background_color: None, ..fill(0x0000ff) }), (10..12, fill(0x0000ff))]
        );
        assert_eq!(lift_fills(&highlights, 12..12), highlights.to_vec(), "nothing selected over them");
    }

    #[test]
    fn a_drag_past_the_scrollers_edge_scrolls_toward_it() {
        let scroller = Bounds::new(point(px(0.), px(100.)), size(px(400.), px(320.)));
        let step = |offset: f32, pointer: f32| autoscroll_offset(scroller, px(offset), px(500.), px(pointer)).map(f32::from);
        assert_eq!(step(0., 300.), None, "inside the scroller");
        assert_eq!(step(0., 430.), Some(-10.), "10px below scrolls down 10px");
        assert_eq!(step(-10., 900.), Some(-58.), "far below, one step is capped");
        assert_eq!(step(-490., 900.), Some(-500.));
        assert_eq!(step(-500., 900.), None, "already at the end");
        assert_eq!(step(-100., 50.), Some(-52.), "above scrolls back up");
        assert_eq!(step(0., 50.), None, "already at the top");
    }
}
