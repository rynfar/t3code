//! The branch picker (`BranchPicker.tsx`, `BranchToolbarBranchSelector.tsx` in panel mode): a
//! searchable list of refs below the branch row. Picking a ref switches the checkout (or moves
//! the thread to the worktree that has it); a query no ref matches offers to create it. Rows are
//! a `uniform_list`, filtered incrementally as the query grows.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, ElementId, Entity, IntoElement, MouseButton, ParentElement, Render, ScrollStrategy, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use t3_core::vcs::refs::RefChoice;
use t3_core::vcs::{RefFilter, VcsRef, branch_selection_target, ref_badge, sanitize_new_ref_name};

use super::{GitEvent, GitStore};
use crate::composer::controls::{Float, floating, menu_surface};
use crate::icons::Icon;
use crate::sidebar::is_composing;
use crate::theme::{hsla, palette};
use crate::thread_details::controls::{MenuRow, ROW_WIDTH};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Ref(VcsRef),
    Create(String),
}

pub struct BranchPicker {
    git: Entity<GitStore>,
    input: Entity<InputState>,
    open: bool,
    highlighted: Option<RefChoice>,
    auto_highlight: bool,
    closed_at: Option<Instant>,
    debounce: Option<Task<()>>,
    filter: RefFilter,
    /// The ref name a "Create" row would make, when the query offers one.
    create: Option<String>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// `MenuRow`'s height and the list's `max-h-56`.
const ROW_HEIGHT: f32 = 28.;
const LIST_MAX_HEIGHT: f32 = 224.;
const LIST_PADDING_Y: f32 = 4.;

impl BranchPicker {
    pub fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.highlighted = None;
                    this.auto_highlight = true;
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    let query = input.read(cx).value().to_string();
                    this.debounce = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| this.git.update(cx, |git, cx| git.load_refs(&query, false, cx)));
                    }));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.refresh_items(cx);
                    if let Some(choice) = this.highlighted.clone() { this.activate(choice, window, cx); }
                },
                _ => {}
            }),
            cx.subscribe(&git, |this, _, _: &GitEvent, cx| {
                if this.open {
                    cx.notify();
                }
            }),
        ];
        BranchPicker {
            git,
            input,
            open: false,
            highlighted: None,
            auto_highlight: true,
            closed_at: None,
            debounce: None,
            filter: RefFilter::default(),
            create: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opening asks for a local status refresh and the first page of refs (`handleOpenChange`).
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return self.close(cx);
        }
        if self.closed_at.take().is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
            return;
        }
        self.open = true;
        self.highlighted = None;
        self.auto_highlight = true;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.git.update(cx, |git, cx| {
            git.refresh_status(true, cx);
            git.load_refs("", false, cx);
        });
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.closed_at = Some(Instant::now());
            self.debounce = None;
            cx.notify();
        }
    }

    /// Brings the filtered rows up to date with the query and the loaded refs; a no-op while
    /// neither changed.
    fn refresh_items(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().trim().to_owned();
        let git = self.git.read(cx);
        self.filter.update(&git.refs.refs, git.refs.revision, &query.to_lowercase());
        let name = sanitize_new_ref_name(&query);
        // An exact match also matches the query, so the filtered rows are enough to rule it out.
        let exists = self.filter.indices().iter().any(|&index| git.refs.refs[index].name == name);
        self.create = (!query.is_empty() && !exists && git.can_change_branch()).then_some(name);
        if self.auto_highlight {
            self.highlighted = RefChoice::at(&git.refs.refs, self.filter.indices(), self.create.as_deref(), 0);
            self.auto_highlight = self.highlighted.is_none();
        }
    }

    fn item_count(&self) -> usize {
        self.filter.indices().len() + usize::from(self.create.is_some())
    }

    fn item(&self, index: usize, cx: &Context<Self>) -> Option<Item> {
        match self.filter.indices().get(index) {
            Some(&ref_index) => self.git.read(cx).refs.refs.get(ref_index).cloned().map(Item::Ref),
            None => self.create.clone().filter(|_| index == self.filter.indices().len()).map(Item::Create),
        }
    }

    fn activate(&mut self, choice: RefChoice, _window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_items(cx);
        let Some(index) = choice.index_in(&self.git.read(cx).refs.refs, self.filter.indices(), self.create.as_deref()) else { return };
        let Some(item) = self.item(index, cx) else { return };
        self.close(cx);
        self.git.update(cx, |git, cx| match &item {
            Item::Ref(target) => git.select_ref(target, cx),
            Item::Create(name) => git.create_ref(name, cx),
        });
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.refresh_items(cx);
        let count = self.item_count();
        if count > 0 {
            let refs = &self.git.read(cx).refs.refs;
            let current = self.highlighted.as_ref().and_then(|choice| choice.index_in(refs, self.filter.indices(), self.create.as_deref()));
            let index = match current {
                Some(index) => (index as isize + delta).rem_euclid(count as isize) as usize,
                None if delta < 0 => count - 1,
                None => 0,
            };
            self.highlighted = RefChoice::at(refs, self.filter.indices(), self.create.as_deref(), index);
            self.auto_highlight = false;
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.item(index, cx) else { return div().into_any_element() };
        let p = palette(cx);
        let id = ElementId::Name(format!("branch-picker-{index}").into());
        let (row, enabled) = match &item {
            Item::Ref(target) => {
                let git = self.git.read(cx);
                let checkout = git.target.as_ref();
                let badge = ref_badge(target, checkout.map(|checkout| checkout.project_cwd.as_str()));
                // Moving to a worktree that already has the ref only records it on the thread.
                let reuses = checkout.is_some_and(|checkout| branch_selection_target(&checkout.project_cwd, checkout.worktree_path.as_deref(), target).reuse_existing_worktree);
                let enabled = git.can_change_branch() || (reuses && git.can_operate_thread());
                let row = MenuRow::new(id, target.name.clone()).trailing(div().children(badge.map(|badge| div().text_size(px(10.)).text_color(hsla(p.muted_foreground).opacity(0.45)).child(badge))));
                (row, enabled)
            }
            Item::Create(name) => (MenuRow::new(id, SharedString::from(format!("Create new ref \"{name}\""))), true),
        };
        let choice = match &item { Item::Ref(item) => RefChoice::Ref(item.name.clone()), Item::Create(name) => RefChoice::Create(name.clone()) };
        let highlighted = self.highlighted.as_ref() == Some(&choice);
        let row = if enabled { row.on_click(cx.listener(move |this, _, window, cx| this.activate(choice.clone(), window, cx))) } else { row };
        div().rounded(px(4.)).when(highlighted, |this| this.bg(hsla(p.accent))).child(row).into_any_element()
    }
}

impl Render for BranchPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        self.refresh_items(cx);
        let p = palette(cx);
        let count = self.item_count();
        let refs = &self.git.read(cx).refs;
        let status_text = if refs.loading && !refs.loaded {
            Some("Loading refs...".to_owned())
        } else if refs.loading_next_page {
            Some("Loading more refs...".to_owned())
        } else if refs.next_cursor.is_some() {
            Some(format!("Showing {} of {} refs", refs.refs.len(), refs.total))
        } else {
            None
        };
        let has_next = refs.next_cursor.is_some() && !refs.loading;
        let error = refs.error.clone();
        let empty = count == 0 && refs.loaded;
        let list_height = (count as f32 * ROW_HEIGHT + 2. * LIST_PADDING_Y).min(LIST_MAX_HEIGHT);
        let popup = menu_surface(cx)
            .id("branch-picker")
            .w(px(ROW_WIDTH))
            .max_h(px(360.))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
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
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .child(Icon::Search.sized(px(16.)).text_color(hsla(p.muted_foreground).opacity(0.55)))
                    .child(div().flex_1().h(px(26.)).flex().items_center().child(Input::new(&self.input).appearance(false).bordered(false))),
            )
            .when(empty, |this| this.child(div().py(px(16.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("No refs found.")))
            .when(count > 0, |this| {
                this.child(
                    uniform_list("branch-picker-list", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()))
                        .track_scroll(&self.scroll)
                        .h(px(list_height))
                        .py(px(LIST_PADDING_Y)),
                )
            })
            .children(error.map(|error| div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error)))
            .children(status_text.map(|text| {
                div()
                    .id("branch-picker-status")
                    .px(px(8.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .when(has_next, |this| {
                        this.cursor_pointer().hover(|style| style.text_color(hsla(p.foreground))).on_click(cx.listener(|this, _, _, cx| {
                            let query = this.input.read(cx).value().to_string();
                            this.git.update(cx, |git, cx| git.load_refs(&query, true, cx));
                        }))
                    })
                    .child(text)
            }));
        floating(Float::BelowStart, div().pt(px(4.)).child(popup))
    }
}
