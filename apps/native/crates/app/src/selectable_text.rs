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

#[cfg(test)]
use gpui::AppContext;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DismissEvent, Element, ElementId, Entity, FocusHandle, Global, GlobalElementId, HighlightStyle,
    Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, RenderOnce, ScrollHandle, SharedString, Styled, StyledText, Subscription, TextLayout,
    TextStyleRefinement, WeakEntity, Window, WrappedLineLayout, div, fill, point, px,
};
use gpui_component::ActiveTheme;
use gpui_component::input::{Copy, SelectAll};
use gpui_component::menu::ContextMenuExt;
use t3_core::text_selection::{Granularity, GroupPoint, GroupSelection, Hit, Selection};

const CONTEXT: &str = "SelectableText";
/// A drag held past the scroller's edge scrolls it this often, by how far past it is, up to
/// `AUTOSCROLL_MAX` a step.
const AUTOSCROLL_STEP: Duration = Duration::from_millis(33);
const AUTOSCROLL_MAX: f32 = 48.;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-c", Copy, Some(CONTEXT)), KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT))]);
}

pub type Highlights = Arc<[(Range<usize>, HighlightStyle)]>;

#[derive(Clone)]
enum ActiveTarget {
    Single(WeakEntity<State>),
    Group(WeakEntity<GroupState>),
}

#[derive(Default)]
struct ActiveSelection(Option<ActiveTarget>);
impl Global for ActiveSelection {}

/// Coordinates drag, click-away, SelectAll, and Copy across multiple sibling `SelectableText`
/// blocks (such as the minimap hover preview's user and assistant paragraphs).
#[derive(Clone)]
pub struct SelectionGroup(Entity<GroupState>);

struct GroupMember {
    part: usize,
    state: WeakEntity<State>,
    bounds: Bounds<Pixels>,
    text: SharedString,
    visible_end: usize,
    layout: Option<TextLayout>,
}

#[derive(Default)]
struct GroupState {
    parts: Vec<GroupMember>,
    selection: Option<GroupSelection>,
    anchor_span: Option<(usize, Range<usize>)>,
    dragging: bool,
    granularity: Option<Granularity>,
}

impl GroupState {
    fn upsert_member(&mut self, member: GroupMember) {
        if let Some(existing) = self.parts.iter_mut().find(|item| item.part == member.part) {
            *existing = member;
        } else {
            self.parts.push(member);
            self.parts.sort_by_key(|item| item.part);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        if selection.is_empty() || self.parts.is_empty() {
            return None;
        }
        let max_part = self.parts.iter().map(|m| m.part).max().unwrap_or(0);
        let mut ordered = vec![""; max_part + 1];
        for member in &self.parts {
            let end = member.visible_end.min(member.text.len());
            ordered[member.part] = &member.text[..end];
        }
        let text = selection.extract_text(&ordered, "\n\n");
        (!text.is_empty()).then_some(text)
    }

    fn contains_point(&self, point: Point<Pixels>) -> bool {
        self.parts.iter().any(|member| member.bounds.contains(&point))
    }

    fn hit_at(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let target = self
            .parts
            .iter()
            .find(|member| position.y <= member.bounds.bottom())
            .or_else(|| self.parts.last())?;
        let layout = target.layout.as_ref()?;
        let lines = layout.line_layouts();
        let hit = Lines::new(&lines, layout).hit(position);
        Some((target.part, hit.caret.min(target.visible_end)))
    }

    fn sync_members(&self, cx: &mut App) {
        for member in &self.parts {
            let range = self.selection.and_then(|selection| selection.range_for_part(member.part, member.visible_end));
            if let Some(state) = member.state.upgrade() {
                state.update(cx, |state, cx| {
                    if state.group_range != range {
                        state.group_range = range;
                        cx.notify();
                    }
                });
            }
        }
    }

    fn clear_selection(&mut self, cx: &mut App) {
        self.selection = None;
        self.anchor_span = None;
        self.dragging = false;
        self.granularity = None;
        self.sync_members(cx);
    }

    #[cfg(test)]
    fn reset(&mut self, cx: &mut App) {
        self.clear_selection(cx);
        self.parts.clear();
    }
}

impl SelectionGroup {
    #[cfg(test)]
    pub fn new(cx: &mut App) -> Self {
        Self(cx.new(|_| GroupState::default()))
    }

    pub fn selected_text(&self, cx: &App) -> Option<String> {
        self.0.read(cx).selected_text()
    }

    pub fn clear(&self, cx: &mut App) {
        self.0.update(cx, |group, cx| group.clear_selection(cx));
        clear_active_selection_if_group(&self.0, cx);
    }

    #[cfg(test)]
    pub fn reset(&self, cx: &mut App) {
        self.0.update(cx, |group, cx| group.reset(cx));
        clear_active_selection_if_group(&self.0, cx);
    }
}

/// Handle to a rendered `SelectableText` instance for reading or clearing its selection.
#[derive(Clone)]
pub struct SelectableTextHandle(Entity<State>);

impl SelectableTextHandle {
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        self.0.read(cx).selected().map(str::to_owned)
    }

    pub fn clear_selection(&self, cx: &mut App) {
        self.0.update(cx, |state, cx| {
            if state.selection.take().is_some() {
                cx.notify();
            }
        });
        clear_active_selection_if(&self.0, cx);
    }

    #[cfg(test)]
    pub fn new_for_test(text: impl Into<SharedString>, cx: &mut App) -> Self {
        use gpui::AppContext;
        let text: SharedString = text.into();
        let entity = cx.new(|cx| State {
            focus: cx.focus_handle(),
            text,
            origin: 0,
            selection: None,
            group: None,
            group_range: None,
            dragging: false,
            menu: None,
            scroll: None,
            layout: None,
            visible_end: None,
            painted_scroll: px(0.),
            pointer: Point::default(),
            autoscrolling: false,
        });
        Self(entity)
    }

    #[cfg(test)]
    pub fn select_all(&self, cx: &mut App) {
        self.0.update(cx, |state, cx| {
            state.selection = Some(Selection::all(&state.text));
            cx.notify();
        });
        set_active_selection(&self.0, cx);
    }
}

pub fn active_selected_text(cx: &App) -> Option<String> {
    match cx.try_global::<ActiveSelection>()?.0.as_ref()? {
        ActiveTarget::Single(weak) => weak.upgrade()?.read(cx).selected().map(str::to_owned),
        ActiveTarget::Group(weak) => weak.upgrade()?.read(cx).selected_text(),
    }
}

pub fn clear_active_selection(cx: &mut App) {
    let Some(target) = cx.try_global::<ActiveSelection>().and_then(|active| active.0.clone()) else {
        return;
    };
    match target {
        ActiveTarget::Single(weak) => {
            if let Some(entity) = weak.upgrade() {
                SelectableTextHandle(entity).clear_selection(cx);
            }
        }
        ActiveTarget::Group(weak) => {
            if let Some(group) = weak.upgrade() {
                SelectionGroup(group).clear(cx);
            }
        }
    }
}

fn set_active_selection(entity: &Entity<State>, cx: &mut App) {
    set_active_target(ActiveTarget::Single(entity.downgrade()), cx);
}

fn set_active_group_selection(group: &Entity<GroupState>, cx: &mut App) {
    set_active_target(ActiveTarget::Group(group.downgrade()), cx);
}

fn set_active_target(target: ActiveTarget, cx: &mut App) {
    if cx.has_global::<ActiveSelection>() {
        cx.global_mut::<ActiveSelection>().0 = Some(target);
    } else {
        cx.set_global(ActiveSelection(Some(target)));
    }
}

fn clear_active_selection_if(entity: &Entity<State>, cx: &mut App) {
    let id = entity.entity_id();
    if cx.has_global::<ActiveSelection>() {
        let active = cx.global_mut::<ActiveSelection>();
        if matches!(active.0.as_ref(), Some(ActiveTarget::Single(weak)) if weak.entity_id() == id) {
            active.0 = None;
        }
    }
}

fn clear_active_selection_if_group(group: &Entity<GroupState>, cx: &mut App) {
    let id = group.entity_id();
    if cx.has_global::<ActiveSelection>() {
        let active = cx.global_mut::<ActiveSelection>();
        if matches!(active.0.as_ref(), Some(ActiveTarget::Group(weak)) if weak.entity_id() == id) {
            active.0 = None;
        }
    }
}

type StateCallback = Arc<dyn Fn(&SelectableTextHandle, &mut App)>;

#[derive(IntoElement)]
pub struct SelectableText {
    id: ElementId,
    text: SharedString,
    highlights: Option<Highlights>,
    origin: usize,
    scroll: Option<ScrollHandle>,
    group: Option<(SelectionGroup, usize)>,
    on_state: Option<StateCallback>,
}

impl SelectableText {
    /// `id` keys the selection, so it stays the same while the text is on screen.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        SelectableText {
            id: id.into(),
            text: text.into(),
            highlights: None,
            origin: 0,
            scroll: None,
            group: None,
            on_state: None,
        }
    }

    #[cfg(test)]
    pub fn selection_group(mut self, group: SelectionGroup, part: usize) -> Self {
        self.group = Some((group, part));
        self
    }

    pub fn on_state(mut self, f: impl Fn(&SelectableTextHandle, &mut App) + 'static) -> Self {
        self.on_state = Some(Arc::new(f));
        self
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

impl SelectionGroup {
    pub fn select_all(&self, cx: &mut App) {
        self.0.update(cx, |group, cx| {
            let (Some(first), Some(last)) = (group.parts.first(), group.parts.last()) else { return };
            group.selection = Some(GroupSelection {
                anchor: GroupPoint { part: first.part, offset: 0 },
                focus: GroupPoint { part: last.part, offset: last.visible_end },
            });
            group.sync_members(cx);
        });
        set_active_group_selection(&self.0, cx);
    }
}

struct State {
    focus: FocusHandle,
    /// The text the selection's offsets index, and where it starts in its stream.
    text: SharedString,
    origin: usize,
    selection: Option<Selection>,
    group: Option<(SelectionGroup, usize)>,
    group_range: Option<Range<usize>>,
    dragging: bool,
    /// Held while this text's context menu is open: a click on the menu, or the one that
    /// dismisses it, must not clear the selection it copies.
    menu: Option<Subscription>,
    scroll: Option<ScrollHandle>,
    /// The last painted layout, the scroll offset it was painted at, and the drag position, for
    /// drag steps between paints.
    layout: Option<TextLayout>,
    visible_end: Option<usize>,
    painted_scroll: Pixels,
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

    fn selected_range(&self) -> Option<Range<usize>> {
        if self.group.is_some() {
            self.group_range.clone().filter(|range| !range.is_empty())
        } else {
            self.selection.as_ref().map(Selection::range).filter(|range| !range.is_empty())
        }
    }

    fn selected(&self) -> Option<&str> {
        self.selected_range().and_then(|range| self.text.get(range))
    }

    /// A drag step at `pointer`: scrolls the scroller when the pointer is past its edge, then
    /// extends the selection to what is under the pointer. Returns whether it scrolled.
    fn drag_to(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        self.pointer = pointer;
        let scrolled = self.scroll.as_ref().is_some_and(|scroll| autoscroll(scroll, pointer));
        if self.extend_to(pointer) || scrolled {
            cx.notify();
        }
        scrolled
    }

    /// Extends the selection to what is under `pointer` now, though the scroller may have moved
    /// the text since the last paint. Returns whether the selection changed.
    fn extend_to(&mut self, pointer: Point<Pixels>) -> bool {
        let Some(layout) = self.layout.as_ref() else { return false };
        let scrolled = self.scroll.as_ref().map_or(px(0.), |scroll| scroll.offset().y - self.painted_scroll);
        let lines = layout.line_layouts();
        let Some(end) = self.visible_end else { return false };
        let hit = Lines::new(&lines, layout).hit_scrolled(pointer, scrolled);
        let hit = Hit { caret: hit.caret.min(end), under: hit.under.min(end) };
        let text = self.text.clone();
        self.selection.as_mut().is_some_and(|selection| selection.extend(&text, hit))
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
    let text = {
        let read = state.read(cx);
        if let Some((group, _)) = &read.group {
            group.selected_text(cx)
        } else {
            read.selected().map(str::to_owned)
        }
    };
    match text {
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
            group: None,
            group_range: None,
            dragging: false,
            menu: None,
            scroll: None,
            layout: None,
            visible_end: None,
            painted_scroll: px(0.),
            pointer: Point::default(),
            autoscrolling: false,
        });
        let group = self.group.clone();
        let (focus, selected) = state.update(cx, |state, _| {
            state.sync(&self.text, self.origin);
            state.scroll = self.scroll;
            if state.group.as_ref().map(|(g, p)| (g.0.entity_id(), *p)) != group.as_ref().map(|(g, p)| (g.0.entity_id(), *p)) {
                state.group_range = None;
            }
            state.group = group.clone();
            (state.focus.clone(), state.selected_range())
        });
        if selected.is_none() {
            clear_active_selection_if(&state, cx);
        }
        if let Some(on_state) = &self.on_state {
            on_state(&SelectableTextHandle(state.clone()), cx);
        }
        let (copy_state, all_state, menu_state, menu_focus) = (state.clone(), state.clone(), state.clone(), focus.clone());
        // The context menu draws inside this div: it gets the app's font, the text keeps the caller's.
        let font_family = window.text_style().font_family;
        div()
            .id(self.id)
            .key_context(CONTEXT)
            .track_focus(&focus)
            .tab_index(0)
            .font_family(cx.theme().font_family.clone())
            .on_action(move |_: &Copy, _, cx| copy(&copy_state, cx))
            .on_action(move |_: &SelectAll, _, cx| {
                let group = all_state.read(cx).group.as_ref().map(|(group, _)| group.clone());
                if let Some(group) = group {
                    group.select_all(cx);
                } else {
                    all_state.update(cx, |state, cx| {
                        state.selection = Some(Selection::all(&state.text));
                        cx.notify();
                    });
                    set_active_selection(&all_state, cx);
                }
            })
            .child(TextElement::new(state, self.text, self.highlights, selected, font_family))
            .context_menu(move |menu, _, cx| {
                let popup = cx.entity();
                let selected = menu_state.update(cx, |state, cx| {
                    state.menu = Some(cx.subscribe(&popup, |state, _, _: &DismissEvent, _| state.menu = None));
                    if let Some((group, _)) = &state.group {
                        group.selected_text(cx).is_some()
                    } else {
                        state.selected().is_some()
                    }
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
        let (range, dragging, selected, group_info) = self.state.update(cx, |state, _| {
            let visible_end = (text_layout.len() == state.text.len()).then_some(state.text.len());
            state.visible_end = visible_end;
            state.layout = Some(text_layout.clone());
            state.painted_scroll = state.scroll.as_ref().map_or(px(0.), |scroll| scroll.offset().y);
            let group_info = state.group.as_ref().map(|(group, part)| {
                (group.clone(), *part, state.text.clone(), visible_end.unwrap_or(0))
            });
            let active_any = state.selected_range().is_some() || state.selection.is_some();
            (state.selected_range(), state.dragging, active_any, group_info)
        });
        if let Some((group, part, text, visible_end)) = group_info {
            let weak_state = self.state.downgrade();
            let member_layout = text_layout.clone();
            group.0.update(cx, |group_state, _| {
                group_state.upsert_member(GroupMember {
                    part,
                    state: weak_state,
                    bounds,
                    text,
                    visible_end,
                    layout: Some(member_layout),
                });
            });
        }
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
                if !phase.bubble() || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                // The row behind must not read the press as a click, nor the window start its own
                // selection from it.
                cx.stop_propagation();
                let (end, focus, group) = {
                    let read = state.read(cx);
                    let Some(end) = read.visible_end else { return };
                    (end, read.focus.clone(), read.group.clone())
                };
                let hit = hit_test(&text_layout, event.position);
                let hit = Hit { caret: hit.caret.min(end), under: hit.under.min(end) };
                window.focus(&focus, cx);
                if let Some((group, part)) = group {
                    let granularity = Granularity::from_clicks(event.click_count);
                    let text = state.read(cx).text.clone();
                    let initial = Selection::press(&text, hit, granularity);
                    group.0.update(cx, |group_state, cx| {
                        if event.modifiers.shift && let Some(selection) = group_state.selection.as_mut() {
                            selection.focus = GroupPoint { part, offset: hit.caret };
                        } else {
                            let initial_range = initial.range();
                            let focus_offset = if initial_range.is_empty() { hit.caret } else { initial_range.end };
                            group_state.selection = Some(GroupSelection {
                                anchor: GroupPoint { part, offset: initial_range.start },
                                focus: GroupPoint { part, offset: focus_offset },
                            });
                            group_state.anchor_span = Some((part, initial_range));
                            group_state.granularity = Some(granularity);
                        }
                        group_state.dragging = true;
                        group_state.sync_members(cx);
                    });
                    state.update(cx, |state, cx| {
                        state.dragging = true;
                        cx.notify();
                    });
                    set_active_group_selection(&group.0, cx);
                } else {
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
                    set_active_selection(&state, cx);
                }
            }
        });
        // Capture phase, so the drag follows the pointer over anything that stops it.
        window.on_mouse_event({
            let state = self.state.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if !phase.capture() || !state.read(cx).dragging {
                    return;
                }
                if let Some((group, _)) = state.read(cx).group.clone() {
                    if event.pressed_button != Some(MouseButton::Left) {
                        state.update(cx, |state, cx| {
                            state.dragging = false;
                            cx.notify();
                        });
                        group.0.update(cx, |group_state, _| group_state.dragging = false);
                    } else {
                        extend_group_selection(&group, event.position, cx);
                    }
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
                    if let Some((group, _)) = state.read(cx).group.clone() {
                        extend_group_selection(&group, event.position, cx);
                        state.update(cx, |state, cx| {
                            state.dragging = false;
                            cx.notify();
                        });
                        let has_selection = group.0.update(cx, |group_state, _| {
                            group_state.dragging = false;
                            group_state.selected_text().is_some()
                        });
                        if has_selection {
                            set_active_group_selection(&group.0, cx);
                        } else {
                            clear_active_selection_if_group(&group.0, cx);
                        }
                        return;
                    }
                    let has_selection = state.update(cx, |state, cx| {
                        // The release settles the end, even where an auto-scroll step left it behind.
                        state.extend_to(event.position);
                        state.dragging = false;
                        cx.notify();
                        state.selected().is_some()
                    });
                    if has_selection {
                        set_active_selection(&state, cx);
                    } else {
                        clear_active_selection_if(&state, cx);
                    }
                }
            }
        });
        if selected {
            window.on_mouse_event({
                let (state, hitbox) = (self.state.clone(), hitbox.clone());
                move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() || hitbox.is_hovered(window) || state.read(cx).menu.is_some() {
                        return;
                    }
                    if let Some((group, _)) = state.read(cx).group.clone() {
                        if group.0.read(cx).contains_point(event.position) {
                            return;
                        }
                        group.clear(cx);
                        return;
                    }
                    state.update(cx, |state, cx| {
                        state.selection = None;
                        cx.notify();
                    });
                    clear_active_selection_if(&state, cx);
                }
            });
        }
    }
}

fn extend_group_selection(group: &SelectionGroup, position: Point<Pixels>, cx: &mut App) {
    group.0.update(cx, |group_state, cx| {
        let Some((part, offset)) = group_state.hit_at(position) else { return };
        let Some(mut selection) = group_state.selection else { return };
        if let Some((anchor_part, span)) = &group_state.anchor_span {
            let next = GroupPoint { part, offset };
            let span_start = GroupPoint { part: *anchor_part, offset: span.start };
            let span_end = GroupPoint { part: *anchor_part, offset: span.end };
            if next < span_start {
                selection.anchor = span_end;
                selection.focus = next;
            } else if next > span_end {
                selection.anchor = span_start;
                selection.focus = next;
            } else {
                selection.anchor = span_start;
                selection.focus = span_end;
            }
        } else {
            selection.focus = GroupPoint { part, offset };
        }
        group_state.selection = Some(selection);
        group_state.sync_members(cx);
    });
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

    /// `hit` for a pointer over text the scroller moved by `scrolled` since these lines were painted.
    fn hit_scrolled(&self, position: Point<Pixels>, scrolled: Pixels) -> Hit {
        self.hit(point(position.x, position.y - scrolled))
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

    #[test]
    fn a_drag_hits_text_where_a_scroll_since_the_last_paint_moved_it() {
        let rows: Vec<_> = (0..6).map(|n| line(&format!("row {n}"), &[])).collect();
        let text = lines(&rows);
        // Painted at the top, then scrolled 40px (two rows) before the pointer moved.
        assert_eq!(text.hit_scrolled(at(10., 5.), px(-40.)).caret, 2 * 6 + 1, "the third row, drawn where the first was");
        assert_eq!(text.hit_scrolled(at(10., 5.), px(0.)).caret, 1);
    }

    #[gpui::test]
    fn cross_paragraph_group_selects_and_copies_across_parts(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Render, size};
        cx.update(gpui_component::init);
        struct Preview {
            group: SelectionGroup,
            show_assistant: bool,
        }
        impl Render for Preview {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .w(px(300.))
                    .flex()
                    .flex_col()
                    .child(SelectableText::new("group-p0", "User prompt text").selection_group(self.group.clone(), 0))
                    .children(self.show_assistant.then(|| {
                        SelectableText::new("group-p1", "Assistant reply text").selection_group(self.group.clone(), 1)
                    }))
            }
        }
        let group = cx.update(SelectionGroup::new);
        let preview_group = group.clone();
        let window = cx.open_window(size(px(320.), px(160.)), |_, _| Preview { group: preview_group, show_assistant: true });
        let root = window.root(cx).unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear());
        let (start, end) = group.0.read_with(visual, |g, _| {
            assert_eq!(g.parts.len(), 2);
            (
                point(g.parts[0].bounds.left() + px(12.), g.parts[0].bounds.center().y),
                point(g.parts[1].bounds.left() + px(100.), g.parts[1].bounds.center().y),
            )
        });
        visual.simulate_mouse_down(start, MouseButton::Left, Default::default());
        visual.simulate_mouse_move(end, Some(MouseButton::Left), Default::default());
        visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
        let selected = visual.update(|_, cx| active_selected_text(cx));
        assert!(
            selected.as_deref().is_some_and(|text| text.contains("\n\n") && text.contains("prompt") && text.contains("Assistant")),
            "expected cross-paragraph selection across both parts, got {selected:?}"
        );
        visual.simulate_mouse_down(point(px(310.), px(150.)), MouseButton::Left, Default::default());
        visual.simulate_mouse_up(point(px(310.), px(150.)), MouseButton::Left, Default::default());
        assert_eq!(visual.update(|_, cx| active_selected_text(cx)), None);

        // Resetting when switching to a turn without assistant text drops stale part: 1.
        visual.update(|window, cx| {
            group.reset(cx);
            root.update(cx, |preview, cx| {
                preview.show_assistant = false;
                cx.notify();
            });
            window.draw(cx).clear();
            group.select_all(cx);
        });
        group.0.read_with(visual, |g, _| assert_eq!(g.parts.len(), 1));
        assert_eq!(visual.update(|_, cx| active_selected_text(cx)).as_deref(), Some("User prompt text"));
    }
}
