//! Branch comparison-base picker for the diff panel header (`DiffPanel.tsx` lines ~567-610 and
//! ~1016-1144). Renders `head → [base picker]` when `DiffScope::Branch` is active and readable,
//! fetching local and remote refs via `vcs.listRefs` on the network runtime.

#![allow(dead_code)] // Wired into `header.rs` by the parent track.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, BoxShadow, Context, ElementId, Entity, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, UniformListScrollHandle, WeakEntity,
    Window, div, point, prelude::*, px, uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{ActiveTheme, h_flex};
use t3_core::diff::{
    BaseRefChoice, BaseRefHighlightKey, BaseRefItem, BaseRefKind, DiffScope, DiffSelection,
    base_ref_items, build_diff_base_ref_choices, list_base_refs_request,
    value_for_base_ref_choice,
};
use t3_core::vcs::VcsRef;

use super::DiffPanel;
use crate::app_state::AppState;
use crate::composer::controls::{Float, floating, popup_surface};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant, focusable, tooltip_view};
use crate::sidebar::is_composing;
use crate::theme::{hsla, palette, rem};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const POPUP_WIDTH: f32 = 288.;
const ROW_HEIGHT: f32 = 28.;
const LIST_MAX_HEIGHT: f32 = 256.;
const LIST_PADDING: f32 = 4.;

#[derive(Clone, PartialEq, Eq)]
struct PickerContext {
    cwd: String,
    connection: Option<u64>,
    head_ref: String,
    base_ref: String,
    selected_base_ref: Option<String>,
}

impl Default for PickerContext {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            connection: None,
            head_ref: "HEAD".to_owned(),
            base_ref: "Automatic".to_owned(),
            selected_base_ref: None,
        }
    }
}

/// Renders the comparison refs (`head → [base picker]`) for `DiffPanel`'s header when branch
/// base selection is available (`canSelectBranchBase`).
pub fn picker(
    panel: &mut DiffPanel,
    window: &mut Window,
    cx: &mut Context<DiffPanel>,
) -> impl IntoElement + use<> {
    let external = panel.is_external();
    let DiffScope::Branch { base_ref } = &panel.scope else {
        return div().into_any_element();
    };
    let selected_base_ref = base_ref.clone();
    let current_parsed = panel.current();

    if !external && let Some(thread_id) = panel.thread_id.as_deref() {
        let state = panel.state.read(cx);
        let thread = state.thread_shell(thread_id);
        let cwd = thread.and_then(|thread| {
            thread.worktree_path.clone().or_else(|| {
                state
                    .shell
                    .project(&thread.project_id)
                    .map(|project| project.workspace_root.clone())
            })
        });
        let can_read_files = state.client_handle().is_some_and(|client| {
            client
                .session_state()
                .borrow()
                .as_ref()
                .is_none_or(|session| session.grants(t3_core::media::FILESYSTEM_READ_SCOPE))
        });
        if state.can_read_threads()
            && can_read_files
            && !panel.is_outside_repository(cx)
            && let Some(cwd) = cwd
        {
            let head_ref = current_parsed
                .as_ref()
                .and_then(|parsed| parsed.head_ref.clone())
                .or_else(|| thread.and_then(|thread| thread.branch.clone()))
                .unwrap_or_else(|| "HEAD".to_owned());
            let base_ref = current_parsed
                .as_ref()
                .and_then(|parsed| parsed.base_ref.clone())
                .or_else(|| selected_base_ref.clone())
                .unwrap_or_else(|| "Automatic".to_owned());
            let connection = panel.connection;
            let panel_weak = cx.entity().downgrade();
            let app_state = panel.state.clone();
            let key = ElementId::NamedInteger(
                "diff-base-ref-picker".into(),
                cx.entity_id().as_u64(),
            );
            let entity = window.use_keyed_state(key, cx, |window, cx| {
                BaseRefPicker::new(panel_weak, app_state, window, cx)
            });
            entity.update(cx, |picker, cx| {
                picker.sync_context(
                    PickerContext {
                        cwd,
                        connection,
                        head_ref,
                        base_ref,
                        selected_base_ref,
                    },
                    cx,
                );
            });
            return entity.into_any_element();
        }
    }

    if !external && let Some(parsed) = panel.view.as_ref().map(|view| view.parsed.clone()) {
        let p = palette(cx);
        let head = parsed.head_ref.clone().unwrap_or_else(|| "HEAD".to_owned());
        let base = parsed
            .base_ref
            .clone()
            .or(selected_base_ref)
            .unwrap_or_else(|| "Automatic".to_owned());
        return div()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(format!("{head} \u{2192} {base}"))
            .into_any_element();
    }

    div().into_any_element()
}

pub struct BaseRefPicker {
    panel: WeakEntity<DiffPanel>,
    state: Entity<AppState>,
    context: PickerContext,
    input: Entity<InputState>,
    last_query: String,
    open: bool,
    closed_at: Option<Instant>,
    highlighted: Option<BaseRefHighlightKey>,
    pointer_selected: bool,
    auto_highlight: bool,
    local_refs: Vec<VcsRef>,
    remote_refs: Vec<VcsRef>,
    loading: bool,
    error: Option<String>,
    request_id: u64,
    debounce: Option<Task<()>>,
    fetch_task: Option<Task<()>>,
    grants_connection: Option<u64>,
    _grants_watch: Option<Task<()>>,
    _refs_watch: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl BaseRefPicker {
    fn new(
        panel: WeakEntity<DiffPanel>,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    if query == this.last_query {
                        return;
                    }
                    this.last_query = query.clone();
                    this.pointer_selected = false;
                    this.auto_highlight = !query.trim().is_empty();
                    this.highlighted = None;
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    this.debounce = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| this.load_refs(&query, cx));
                    }));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if !is_composing(&this.input, window, cx) => {
                    this.activate_highlighted(cx);
                }
                _ => {}
            },
        )];
        Self {
            panel,
            state,
            context: PickerContext::default(),
            input,
            last_query: String::new(),
            open: false,
            closed_at: None,
            highlighted: None,
            pointer_selected: false,
            auto_highlight: false,
            local_refs: Vec::new(),
            remote_refs: Vec::new(),
            loading: false,
            error: None,
            request_id: 0,
            debounce: None,
            fetch_task: None,
            grants_connection: None,
            _grants_watch: None,
            _refs_watch: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn sync_context(&mut self, next: PickerContext, cx: &mut Context<Self>) {
        let connection_changed = self.context.connection != next.connection;
        let cwd_changed = self.context.cwd != next.cwd;
        self.context = next;

        if connection_changed || self.grants_connection != self.context.connection {
            self.grants_connection = self.context.connection;
            self.setup_client_watchers(cx);
        }

        if connection_changed || cwd_changed {
            self.local_refs.clear();
            self.remote_refs.clear();
            self.loading = false;
            self.error = None;
            self.request_id += 1;
            self.debounce = None;
            self.fetch_task = None;
            if self.open {
                let query = self.last_query.clone();
                self.load_refs(&query, cx);
            }
        }
    }

    fn setup_client_watchers(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            self._grants_watch = None;
            self._refs_watch = None;
            return;
        };

        let mut grants = client.session_state();
        let panel_for_grants = self.panel.clone();
        self._grants_watch = Some(cx.spawn(async move |_, cx| {
            while grants.changed().await.is_ok() {
                grants.borrow_and_update();
                let Some(panel) = panel_for_grants.upgrade() else {
                    return;
                };
                panel.update(cx, |_, cx| cx.notify());
            }
        }));

        let mut revisions = client.refs_revision();
        self._refs_watch = Some(cx.spawn(async move |this, cx| {
            while revisions.changed().await.is_ok() {
                revisions.borrow_and_update();
                let ok = this
                    .update(cx, |this, cx| {
                        if this.open {
                            let query = this.last_query.clone();
                            this.load_refs(&query, cx);
                        }
                    })
                    .is_ok();
                if !ok {
                    return;
                }
            }
        }));
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
            return;
        }
        if self
            .closed_at
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.open = true;
        self.highlighted = None;
        self.pointer_selected = false;
        self.auto_highlight = false;
        self.last_query.clear();
        self.debounce = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.load_refs("", cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.closed_at = Some(Instant::now());
        self.debounce = None;
        self.highlighted = None;
        self.pointer_selected = false;
        self.auto_highlight = false;
        self.last_query.clear();
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    fn load_refs(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            return;
        };
        let cwd = self.context.cwd.clone();
        if cwd.is_empty() {
            return;
        }
        let runtime = self.state.read(cx).runtime().clone();
        let connection = self.context.connection;
        let query_trimmed = query.trim().to_owned();
        self.request_id += 1;
        let request_id = self.request_id;
        self.loading = true;
        self.error = None;
        let revisions = client.refs_revision();
        let revision = *revisions.borrow();
        let sessions = client.connection_generation();
        let generation = *sessions.borrow();
        let local_payload =
            list_base_refs_request(&cwd, BaseRefKind::Local, Some(&query_trimmed));
        let remote_payload =
            list_base_refs_request(&cwd, BaseRefKind::Remote, Some(&query_trimmed));

        let task = runtime.spawn(async move {
            let (local_result, remote_result) = tokio::join!(
                client.list_refs(local_payload),
                client.list_refs(remote_payload),
            );
            let local = local_result?.map(|result| result.refs);
            let remote = remote_result?.map(|result| result.refs);
            Ok::<_, String>((local, remote))
        });

        cx.notify();
        self.fetch_task = Some(cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Failed to load refs.".into()));
            let _ = this.update(cx, |this, cx| {
                if this.request_id != request_id
                    || this.context.connection != connection
                    || this.context.cwd != cwd
                    || *sessions.borrow() != generation
                {
                    return;
                }
                if *revisions.borrow() != revision {
                    if this.open {
                        let current_query = this.last_query.clone();
                        this.load_refs(&current_query, cx);
                    }
                    return;
                }
                this.loading = false;
                match result {
                    Ok((Some(local), Some(remote))) => {
                        this.local_refs = local;
                        this.remote_refs = remote;
                        this.error = None;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn current_items(&self) -> Vec<BaseRefItem> {
        let choices = build_diff_base_ref_choices(
            &self.local_refs,
            &self.remote_refs,
            Some(&self.context.head_ref),
        );
        base_ref_items(&choices, &self.last_query)
    }

    fn resolve_highlighted(&self, items: &[BaseRefItem]) -> Option<usize> {
        if items.is_empty() {
            return None;
        }
        if let Some(key) = &self.highlighted
            && let Some(index) = items.iter().position(|item| &item.key() == key)
        {
            return Some(index);
        }
        if self.auto_highlight { Some(0) } else { None }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let items = self.current_items();
        let count = items.len();
        if count == 0 {
            return;
        }
        let current = self.resolve_highlighted(&items);
        let next_index = match current {
            Some(index) => (index as isize + delta).rem_euclid(count as isize) as usize,
            None if delta < 0 => count - 1,
            None => 0,
        };
        self.highlighted = Some(items[next_index].key());
        self.pointer_selected = false;
        self.auto_highlight = false;
        self.scroll.scroll_to_item(next_index, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn activate_highlighted(&mut self, cx: &mut Context<Self>) {
        let items = self.current_items();
        let Some(index) = self.resolve_highlighted(&items) else {
            return;
        };
        let value = items[index].selection_value(self.context.selected_base_ref.as_deref());
        self.select_base_ref(value, true, cx);
    }

    fn select_base_ref(&mut self, base_ref: Option<String>, close_popup: bool, cx: &mut Context<Self>) {
        if close_popup {
            self.open = false;
            self.closed_at = Some(Instant::now());
            self.debounce = None;
            self.highlighted = None;
            self.pointer_selected = false;
            self.auto_highlight = false;
        }
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| {
                panel.select(DiffSelection::Branch { base_ref }, cx);
            });
        }
        cx.notify();
    }

    fn render_row(
        &self,
        index: usize,
        item: &BaseRefItem,
        highlighted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let selected_ref = self.context.selected_base_ref.as_deref();
        let (label, selected, key, selection_value): (
            SharedString,
            bool,
            BaseRefHighlightKey,
            Option<String>,
        ) = match item {
            BaseRefItem::Automatic => (
                "Automatic".into(),
                selected_ref.is_none(),
                item.key(),
                None,
            ),
            BaseRefItem::Choice(choice) => {
                let value = value_for_base_ref_choice(choice, selected_ref);
                let is_selected = selected_ref == Some(value);
                (
                    choice.label.clone().into(),
                    is_selected,
                    item.key(),
                    Some(value.to_owned()),
                )
            }
        };

        let remote_control: Option<AnyElement> = match item {
            BaseRefItem::Choice(choice) => render_remote_control(choice, index, selected_ref, window, cx),
            BaseRefItem::Automatic => None,
        };

        let row_bg = if highlighted {
            Some(hsla(p.accent))
        } else if selected {
            Some(hsla(p.foreground).opacity(0.08))
        } else {
            None
        };
        let row_fg = if highlighted {
            hsla(p.accent_foreground)
        } else {
            hsla(p.foreground)
        };
        let check_color = if highlighted {
            hsla(p.accent_foreground)
        } else {
            hsla(p.muted_foreground)
        };

        let hover_key = key;
        let row = div()
            .id(("diff-base-ref-row", index))
            .w_full()
            .flex()
            .items_center()
            .min_h(px(ROW_HEIGHT))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .text_color(row_fg)
            .when_some(row_bg, |this, bg| this.bg(bg))
            .cursor_pointer()
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let changed =
                    !this.pointer_selected || this.highlighted.as_ref() != Some(&hover_key);
                this.highlighted = Some(hover_key.clone());
                this.pointer_selected = true;
                this.auto_highlight = false;
                if changed {
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.select_base_ref(selection_value.clone(), true, cx);
                this.input.update(cx, |input, cx| input.set_value("", window, cx));
                this.last_query.clear();
            }))
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |this| {
                        this.child(Icon::Check.svg().size(px(14.)).text_color(check_color))
                    }),
            )
            .child(div().flex_1().min_w_0().truncate().pr(px(8.)).child(label))
            .child(
                div()
                    .w(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(remote_control),
            );

        div().w_full().child(row).into_any_element()
    }
}

fn render_remote_control(
    choice: &BaseRefChoice,
    index: usize,
    selected_base_ref: Option<&str>,
    window: &mut Window,
    cx: &mut Context<BaseRefPicker>,
) -> Option<AnyElement> {
    let p = palette(cx);
    let value = value_for_base_ref_choice(choice, selected_base_ref);
    if let (Some(local), Some(remote)) = (&choice.local, &choice.remote) {
        let use_remote = remote.name == value;
        let local_name = local.name.clone();
        let remote_name = remote.name.clone();
        let switch_id = ElementId::NamedInteger("diff-base-ref-remote-switch".into(), index as u64);
        let picker = cx.entity().downgrade();
        return Some(
            mini_switch(
                switch_id,
                use_remote,
                move |checked, _, cx| {
                    let next_ref = if checked {
                        remote_name.clone()
                    } else {
                        local_name.clone()
                    };
                    if let Some(picker) = picker.upgrade() {
                        picker.update(cx, |this, cx| {
                            this.select_base_ref(Some(next_ref), false, cx)
                        });
                    }
                },
                window,
                cx,
            )
            .into_any_element(),
        );
    }

    if choice.remote.is_some() {
        return Some(
            div()
                .id(("diff-base-ref-remote-only", index))
                .flex()
                .items_center()
                .justify_end()
                .text_color(hsla(p.muted_foreground))
                .tooltip(|window, cx| tooltip_view("Remote only".into(), window, cx))
                .child(Icon::Check.svg().size(px(12.)))
                .into_any_element(),
        );
    }

    None
}

/// Embedded mini switch (`switch.tsx` with `[--thumb-size:--spacing(3)]`, design-system §6.3/§6.7):
/// 22×14 track, 10px thumb traveling 8px.
fn mini_switch(
    id: impl Into<ElementId>,
    checked: bool,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let thumb = if dark && !checked {
        hsla(p.muted_foreground)
    } else {
        hsla(p.background)
    };
    let next = !checked;
    let id = id.into();
    focusable(div().id(id.clone()), id, true, window, cx)
        .flex()
        .flex_none()
        .items_center()
        .w(rem(22.))
        .h(rem(14.))
        .p(rem(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if checked {
            hsla(p.primary)
        } else {
            hsla(p.input)
        })
        .child(
            div()
                .size(rem(10.))
                .rounded_full()
                .bg(thumb)
                .when(checked, |this| this.ml(rem(8.)))
                .shadow(vec![BoxShadow {
                    color: gpui::black().opacity(0.05),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(3.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(next, window, cx);
        })
}

impl Render for BaseRefPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let head_ref: SharedString = self.context.head_ref.clone().into();
        let base_ref: SharedString = self.context.base_ref.clone().into();
        let pair_tooltip: SharedString =
            format!("{} \u{2192} {}", self.context.head_ref, self.context.base_ref).into();
        let trigger_tooltip: SharedString =
            format!("Change comparison target. Currently {}", self.context.base_ref).into();

        let popup = self.open.then(|| {
            let items = self.current_items();
            let count = items.len();
            let highlighted_index = self.resolve_highlighted(&items);
            let empty = count == 0;
            let list_height = (count as f32 * ROW_HEIGHT).min(LIST_MAX_HEIGHT);
            let error = self.error.clone();
            let items = std::sync::Arc::new(items);

            let surface = popup_surface(cx)
                .id("diff-base-ref-popup")
                .p(px(0.))
                .w(px(POPUP_WIDTH))
                .max_h(px(368.))
                .overflow_hidden()
                .occlude()
                .capture_key_down(cx.listener(|this, _, _, _| {
                    this.pointer_selected = false;
                }))
                .on_mouse_move(cx.listener(|this, _, _, cx| {
                    if this.pointer_selected {
                        this.highlighted = None;
                        this.pointer_selected = false;
                        cx.notify();
                    }
                }))
                .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                    if !inside && this.pointer_selected {
                        this.highlighted = None;
                        this.pointer_selected = false;
                        cx.notify();
                    }
                }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
                .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                    if !is_composing(&this.input, window, cx) {
                        this.move_highlight(1, cx);
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                    if !is_composing(&this.input, window, cx) {
                        this.move_highlight(-1, cx);
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    if !is_composing(&this.input, window, cx) {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && !is_composing(&this.input, window, cx) {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(12.))
                        .pt(px(10.))
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .child(
                            Icon::Search
                                .sized(px(16.))
                                .text_color(hsla(p.muted_foreground).opacity(0.55)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .child(Input::new(&self.input).appearance(false).bordered(false)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .pt(px(8.))
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.muted_foreground))
                        .child(div().w(px(16.)).flex_none())
                        .child(div().flex_1().min_w_0().child("BRANCH"))
                        .child(div().w(px(32.)).flex_none().flex().justify_end().child("REMOTE")),
                )
                .when(empty, |this| {
                    this.child(
                        div()
                            .p(px(8.))
                            .flex()
                            .justify_center()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(if self.loading {
                                "Loading refs..."
                            } else {
                                "No matching refs."
                            }),
                    )
                })
                .when(count > 0, |this| {
                    this.child(
                        div().p(px(LIST_PADDING)).child(
                            uniform_list(
                                "diff-base-ref-list",
                                count,
                                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                                    range
                                        .map(|index| {
                                            this.render_row(
                                                index,
                                                &items[index],
                                                highlighted_index == Some(index),
                                                window,
                                                cx,
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .h(px(list_height)),
                        ),
                    )
                })
                .children(error.map(|error| {
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .text_xs()
                        .text_color(hsla(p.destructive_foreground))
                        .child(error)
                }));

            floating(Float::BelowStart, div().pt(px(4.)).child(surface))
        });

        let _ = window;
        h_flex()
            .min_w_0()
            .max_w_full()
            .items_center()
            .gap(px(8.))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(
                div()
                    .id("diff-base-head-ref")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .tooltip(move |window, cx| tooltip_view(pair_tooltip.clone(), window, cx))
                    .child(div().min_w_0().max_w(px(192.)).truncate().child(head_ref))
                    .child(
                        Icon::ArrowRight
                            .svg()
                            .size(px(14.))
                            .text_color(hsla(p.muted_foreground).opacity(0.7)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        Button::new("diff-base-ref-trigger")
                            .variant(Variant::GhostMuted)
                            .size(Size::Xs)
                            .label(base_ref)
                            .trailing(Icon::ChevronDown)
                            .pressed(self.open)
                            .tooltip(trigger_tooltip)
                            .min_w(px(36.))
                            .max_w(px(192.))
                            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
                    )
                    .children(popup),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, point, size};

    use super::*;

    fn local(name: &str) -> VcsRef {
        VcsRef {
            name: name.to_owned(),
            is_remote: Some(false),
            remote_name: None,
            current: name == "dev",
            is_default: name == "main",
            worktree_path: None,
        }
    }

    fn remote(name: &str) -> VcsRef {
        VcsRef {
            name: name.to_owned(),
            is_remote: Some(true),
            remote_name: Some("origin".to_owned()),
            current: false,
            is_default: false,
            worktree_path: None,
        }
    }

    #[gpui::test]
    fn picker_renders_filters_and_selects_branch_base(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| {
                let mut state = AppState::new(runtime.handle().clone(), cx);
                state.selected_thread = Some("thread-1".to_owned());
                state
            })
        });
        let (panel, cx) = cx.add_window_view(|_, cx| DiffPanel::detached(state.clone(), cx));
        let panel_weak = panel.downgrade();
        let picker = cx.update(|window, cx| {
            cx.new(|cx| BaseRefPicker::new(panel_weak, state.clone(), window, cx))
        });

        picker.update(cx, |picker, cx| {
            picker.sync_context(
                PickerContext {
                    cwd: "/repo".to_owned(),
                    connection: None,
                    head_ref: "dev".to_owned(),
                    base_ref: "Automatic".to_owned(),
                    selected_base_ref: None,
                },
                cx,
            );
            picker.local_refs = vec![local("dev"), local("main")];
            picker.remote_refs = vec![remote("origin/main"), remote("origin/release")];
            let items = picker.current_items();
            assert_eq!(items.len(), 3);
            assert_eq!(items[0], BaseRefItem::Automatic);
        });

        cx.draw(point(px(0.), px(0.)), size(px(800.), px(400.)), |_, _| {
            picker.clone().into_any_element()
        });

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.toggle(window, cx));
        });
        // Skip drawing the open popover under `#[gpui::test]`: `gpui_component::input::Input`
        // queries the macOS platform window handle, which `TestWindow` intentionally leaves unimplemented.

        picker.update(cx, |picker, cx| {
            assert!(picker.open);
            picker.move_highlight(1, cx);
            picker.move_highlight(1, cx);
            picker.activate_highlighted(cx);
            assert!(!picker.open);
        });
        assert_eq!(
            state.read_with(cx, |state, _| state.panels.diff_selection("thread-1")),
            DiffSelection::Branch { base_ref: Some("main".to_owned()) }
        );

        picker.update(cx, |picker, cx| {
            picker.last_query = "origin/".to_owned();
            assert_eq!(picker.current_items().len(), 2);
            picker.last_query = "does-not-exist".to_owned();
            assert!(picker.current_items().is_empty());
            picker.select_base_ref(None, false, cx);
        });
        assert_eq!(
            state.read_with(cx, |state, _| state.panels.diff_selection("thread-1")),
            DiffSelection::Branch { base_ref: None }
        );
    }
}
