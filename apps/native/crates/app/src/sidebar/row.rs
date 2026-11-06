//! Row rendering (design-system §7.1–7.3, `sidebar/SidebarThreadRow.tsx`,
//! `sidebar/SidebarListMarkers.tsx`): 78px cards for pinned, active and working threads, 36px slim
//! rows for snoozed and settled ones, shelf headers, and the Settled "Show more" button.

use gpui::{AnyElement, App, BoxShadow, Context, DragMoveEvent, FontWeight, Hsla, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, point, prelude::*, px};
use gpui_component::Sizable as _;
use t3_core::sidebar::settled::{self, snooze_wake_label};
use t3_core::sidebar::status::{self, StatusIcon, StatusTone, ThreadStatus, TopStatus};
use t3_core::sidebar::thread::SidebarThread;
use t3_core::sidebar::time::{compact_relative_label, working_duration_label};
use t3_core::sidebar::{Section, Shelf, SidebarRow, sort::settled_timestamp};

use super::SidebarPane;
use super::drag::{DraggedThread, ThreadDragPreview, ThreadHoverCard};
use crate::icons::Icon;
use crate::theme::{Palette, hsla, palette};

const CARD_CONTENT_HEIGHT: f32 = 78.;
const SLIM_HEIGHT: f32 = 36.;
const SHELF_HEIGHT: f32 = 32.;

fn status_icon(icon: StatusIcon) -> Icon {
    match icon {
        StatusIcon::Working => Icon::CircleDashed,
        StatusIcon::Waiting => Icon::CircleDotDashed,
        StatusIcon::Input => Icon::MessageCircleQuestion,
        StatusIcon::Request => Icon::UserRound,
        StatusIcon::Approval => Icon::ShieldQuestion,
        StatusIcon::Failed => Icon::CircleAlert,
        StatusIcon::Done => Icon::CircleCheck,
        StatusIcon::Woke => Icon::AlarmClock,
    }
}

fn tone_color(tone: StatusTone, p: Palette) -> u32 {
    match tone {
        StatusTone::Info => p.info_foreground,
        StatusTone::Muted => p.muted_foreground,
        StatusTone::Warning => p.warning_foreground,
        StatusTone::Input => p.status_input_icon,
        StatusTone::Guest => p.guest_foreground,
        StatusTone::Error => p.error_foreground,
        StatusTone::Success => p.success_foreground,
    }
}

struct RowState {
    status: ThreadStatus,
    top: Option<TopStatus>,
    recede: bool,
    unread: bool,
    woke_at: Option<i64>,
    is_woke: bool,
}

fn row_state(thread: &SidebarThread, active: bool, now: i64) -> RowState {
    let status = status::resolve_status(thread);
    let unread = status::has_unseen_completion(thread);
    let woke_at = settled::woke_at(thread, now);
    let is_woke = woke_at.is_some_and(|woke| thread.last_visited_at.is_none_or(|visited| visited < woke)) && !thread.is_settled();
    RowState {
        status,
        top: status::top_status(status, thread.goal_active, is_woke, unread),
        recede: status::should_recede(status, unread, is_woke, active),
        unread,
        woke_at,
        is_woke,
    }
}

/// A 12px text action revealed on hover (design-system §7.2 hover actions).
fn hover_action(id: SharedString, p: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h_full()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(6.))
        .rounded(px(6.))
        .text_xs()
        .text_color(hsla(p.muted_foreground))
        .cursor_pointer()
        .hover(|this| this.text_color(hsla(p.foreground)))
}

impl SidebarPane {
    pub(super) fn render_row(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if index < self.drafts.rows.len() { return self.render_draft_row(index, cx); }
        let index = if self.drafts.rows.is_empty() { index } else {
            if index == self.drafts.rows.len() {
                return div().h(px(13.)).mx(px(10.)).py(px(6.)).child(div().h(px(1.)).bg(hsla(palette(cx).sidebar_border).opacity(0.6))).into_any_element();
            }
            index - self.drafts.rows.len() - 1
        };
        let app = self.state.read(cx);
        if !app.fleet.environments.is_empty() {
            let model = app.sidebar.fleet.clone();
            let Some(row) = model.rows.get(index) else { return div().into_any_element() };
            let element = match row {
                t3_core::sidebar::fleet::ScopedSidebarRow::Thread { index, section } => {
                    let row = &model.threads[*index];
                    let project = model.project_names.get(&row.project).cloned();
                    let active = app.active_thread_key(&row.key.thread_id).as_ref() == Some(&row.key) && app.selected_thread.as_deref() == Some(row.key.thread_id.as_str());
                    if section.is_card() { self.render_card(Some(&row.key), &row.thread, project, *section, active, cx) }
                    else { self.render_slim(Some(&row.key), &row.thread, project, *section, active, cx) }
                }
                t3_core::sidebar::fleet::ScopedSidebarRow::Shelf { shelf, count, expanded } => self.render_shelf(*shelf, *count, *expanded, cx),
                t3_core::sidebar::fleet::ScopedSidebarRow::ShowMore { count } => self.render_show_more(*count, cx),
                t3_core::sidebar::fleet::ScopedSidebarRow::Marker(marker) => self.render_marker(*marker, cx),
                t3_core::sidebar::fleet::ScopedSidebarRow::Folder { key, expanded, hidden_count, dot, .. } => self.render_project_folder(key, *expanded, *hidden_count, *dot, cx),
            };
            return div().id(("scoped-sidebar-row", index)).relative()
                .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedThread>, _, cx| this.drag_moved(index, event, cx)))
                .child(element).children(self.drop_line(index, cx)).into_any_element();
        }
        let model = app.sidebar.model.clone();
        let Some(row) = model.rows.get(index) else { return div().into_any_element() };
        let element = match row {
            SidebarRow::Thread { index: thread_index, section } => {
                let thread = &model.threads[*thread_index];
                let project = model.project_names.get(&thread.project_id).cloned();
                let active = self.state.read(cx).selected_thread.as_deref() == Some(thread.id.as_str());
                if section.is_card() {
                    self.render_card(None, thread, project, *section, active, cx)
                } else {
                    self.render_slim(None, thread, project, *section, active, cx)
                }
            }
            SidebarRow::Shelf { shelf, count, expanded } => self.render_shelf(*shelf, *count, *expanded, cx),
            SidebarRow::ShowMore { count } => self.render_show_more(*count, cx),
            SidebarRow::Marker(marker) => self.render_marker(*marker, cx),
        };
        div()
            .id(("sidebar-row", index))
            .relative()
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedThread>, _, cx| this.drag_moved(index, event, cx)))
            .child(element)
            .children(self.drop_line(index, cx))
            .into_any_element()
    }

    fn row_surface(&self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, project: Option<String>, section: Section, active: bool, recede_working: bool, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = palette(cx);
        let light = !cx.window_appearance_is_dark();
        let id = thread.id.clone();
        let menu_id = thread.id.clone();
        let clicked_key = key.cloned();
        let menu_key = key.cloned();
        let state = self.state.read(cx);
        let selected = !active && key.map_or_else(|| state.sidebar.selection.contains(&thread.id), |key| state.sidebar.fleet_selection.contains(key));
        let hover_bg = !active && !selected;
        let dragging = key.map_or_else(|| state.sidebar.drag.as_ref().is_some_and(|(dragged, _)| *dragged == thread.id), |key| state.sidebar.fleet_drag.as_ref().is_some_and(|(dragged, _)| dragged == key));
        let draggable = section != Section::Working && key.map_or(state.sidebar.capabilities.can_operate, |key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key).is_some_and(|owner| owner.can_operate_owner(state))) && self.rename.as_ref().is_none_or(|(renaming, _)| *renaming != thread.id);
        let ring = BoxShadow { color: hsla(p.sidebar_border).opacity(0.7), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: true };
        let visual = key.map_or_else(|| crate::project_icon::ProjectVisual::for_project(state, &thread.project_id), |key| crate::project_icon::ProjectVisual::for_scoped_project(state, &t3_core::sidebar::grouping::ProjectRef { environment_id: key.environment_id.clone(), project_id: thread.project_id.clone() }));
        let card = ThreadHoverCard { title: thread.title.clone().into(), project: project.clone().map(Into::into), favicon: visual.clone(), branch: thread.branch.clone().map(Into::into), model: key.is_none().then(|| state.thread_shell(&thread.id).map(|shell| shell.model_selection.model.clone()).filter(|model| !model.is_empty())).flatten().map(Into::into), error: thread.runtime.as_ref().filter(|runtime| runtime.last_error.is_some()).map(|runtime| runtime.last_error_class.as_deref() == Some("usage_limit")) };
        let preview = (thread.title.clone(), project, visual);
        let pane = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("thread-row-{:?}-{}", key, thread.id)))
            .group("sidebar-row")
            .relative()
            .w_full()
            .rounded(px(6.))
            .cursor_pointer()
            .when(active, |this| this.bg(hsla(p.sidebar_row_active)).when(light, |this| this.shadow(vec![ring.clone()])))
            .when(selected, |this| this.bg(hsla(p.sidebar_row_selected)).when(light, |this| this.shadow(vec![ring.clone()])))
            .when(recede_working, |this| this.opacity(0.7))
            // GPUI allows one hover style per element: the background and the receding row's full opacity share it.
            .when(hover_bg || recede_working, |this| {
                this.hover(|style| {
                    let style = if hover_bg { style.bg(hsla(p.sidebar_row_hover)) } else { style };
                    if recede_working { style.opacity(1.) } else { style }
                })
            })
            .when(dragging, |this| this.opacity(0.5))
            .when(self.drag.is_none() && !self.menu_open, |this| this.tooltip(move |_, cx| {
                cx.new(|_| ThreadHoverCard { title: card.title.clone(), project: card.project.clone(), favicon: card.favicon.clone(), branch: card.branch.clone(), model: card.model.clone(), error: card.error }).into()
            }))
            .when(draggable, |this| {
                this.on_drag(DraggedThread { id: thread.id.clone(), key: key.cloned() }, move |dragged, _, _, cx| {
                    let preview = cx.new(|_| ThreadDragPreview::new(preview.0.clone(), preview.1.clone(), preview.2.clone()));
                    if let Some(pane) = pane.upgrade() {
                        pane.update(cx, |pane, cx| pane.begin_drag(&dragged.id, dragged.key.as_ref(), preview.clone(), cx));
                    }
                    preview
                })
            })
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                if event.click_count() > 1 && !modifiers.shift && !modifiers.platform && !modifiers.control {
                    if let Some(key) = &clicked_key { this.start_scoped_rename(key, window, cx); }
                    else { this.start_rename(&id, window, cx); }
                    return;
                }
                this.state.update(cx, |state, cx| match &clicked_key {
                    Some(key) => state.click_scoped_sidebar_thread(key, modifiers, cx),
                    None => state.click_sidebar_thread(&id, modifiers, cx),
                });
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                if let Some(key) = &menu_key {
                    let selected = this.state.read(cx).sidebar.fleet_selection.contains(key);
                    this.menu.update(cx, |menu, cx| if selected { menu.open_for_selection(event.position, window, cx); } else { menu.open_for_scoped_thread(key, event.position, window, cx); });
                }
                else { this.open_menu(&menu_id, event.position, window, cx); }
                cx.stop_propagation();
            }))
    }

    fn render_title(&self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, color: Hsla, recede: bool, cx: &mut Context<Self>) -> AnyElement {
        if let Some((_, input)) = self.rename.as_ref().filter(|(id, _)| *id == thread.id && self.rename_key.as_ref() == key) {
            let p = palette(cx);
            return div()
                .id("thread-rename")
                .capture_action(cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    let composing = this.rename.as_ref().is_some_and(|(_, input)| super::is_composing(input, window, cx));
                    if !composing {
                        this.cancel_rename(cx);
                        cx.stop_propagation();
                    }
                }))
                .flex_1()
                .min_w_0()
                .h(crate::theme::rem(20.))
                .flex()
                .items_center()
                .px(crate::theme::rem(4.))
                .rounded(px(4.))
                .border_1()
                .border_color(hsla(p.foreground))
                .bg(hsla(p.card))
                .text_color(hsla(p.card_foreground))
                .font_weight(FontWeight::MEDIUM)
                // The input's own height and padding would not fit the field: one `text-sm` line, centered.
                .child(gpui_component::input::Input::new(input).appearance(false).bordered(false).xsmall().px_0().h_full().text_sm())
                .into_any_element();
        }
        let title: SharedString = thread.title.clone().into();
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_sm()
            .font_weight(if recede { FontWeight::NORMAL } else { FontWeight::MEDIUM })
            .text_color(color)
            .when(thread.regenerating_title, |this| this.opacity(0.55))
            .child(title)
            .into_any_element()
    }

    fn pin_indicator(&self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !thread.pinned {
            return None;
        }
        let p = palette(cx);
        let id = thread.id.clone();
        let key = key.cloned();
        Some(
            div()
                .id(SharedString::from(format!("unpin-{}", thread.id)))
                .group("unpin")
                .flex_none()
                .cursor_pointer()
                .child(Icon::Pin.sized(px(12.)).text_color(hsla(p.muted_foreground).opacity(0.65)).group_hover("unpin", |this| this.invisible()))
                .child(div().absolute().top_0().invisible().group_hover("unpin", |this| this.visible()).child(Icon::PinOff.sized(px(12.)).text_color(hsla(p.foreground))))
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(key) = &key {
                        if let Some(owner) = crate::context_menu::ThreadMenuOwner::capture_scoped(this.state.read(cx),key) { this.menu.update(cx,|menu,cx| menu.confirm_scoped(owner,t3_core::thread_actions::ThreadAction::Unpin,cx)); }
                    } else { this.state.update(cx,|state,cx| state.request_unpin_thread(&id,cx)); }
                }))
                .into_any_element(),
        )
    }

    fn render_card(&mut self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, project: Option<String>, section: Section, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let now = self.now;
        let state = row_state(thread, active, now);
        let show_snooze = super::snooze_access(self.state.read(cx)) && settled::can_snooze(thread, now);
        let title_color = if state.recede {
            hsla(p.foreground).opacity(0.75)
        } else if state.status == ThreadStatus::Failed && !state.unread && !state.is_woke {
            hsla(p.foreground).opacity(0.95)
        } else {
            hsla(p.foreground)
        };
        let status_slot = match &state.top {
            Some(top) if top.icon == StatusIcon::Woke => {
                let id = thread.id.clone();
                let woke_at = state.woke_at;
                div()
                    .id(SharedString::from(format!("woke-{}", thread.id)))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(tone_color(top.tone, p)))
                    .child(Icon::AlarmClock.sized(px(16.)))
                    .child(top.label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(woke_at) = woke_at {
                            this.state.update(cx, |state, cx| state.acknowledge_woke(&id, woke_at, cx));
                        }
                    }))
                    .into_any_element()
            }
            Some(top) => {
                let duration = match state.status {
                    ThreadStatus::Preparing | ThreadStatus::Working => status::working_started_at(thread),
                    ThreadStatus::Waiting => thread.latest_run.as_ref().and_then(|run| run.completed_at),
                    _ => None,
                }
                .map(|started| working_duration_label(now - started));
                if duration.is_some() {
                    self.note_live_row(cx);
                }
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(tone_color(top.tone, p)))
                    .child(status_icon(top.icon).sized(px(16.)))
                    .child(top.label)
                    .children(duration)
                    .into_any_element()
            }
            None => div()
                .text_color(hsla(p.sidebar_secondary_label))
                .children(thread.latest_user_message_at.or(thread.updated_at).map(|at| compact_relative_label(at, now)))
                .into_any_element(),
        };
        let settle_id = thread.id.clone();
        let snooze_id = thread.id.clone();
        let settle_key = key.cloned();
        let snooze_key = key.cloned();
        let actions = div()
            .when(!state.is_woke, |this| this.absolute().right_0().top_0().bottom_0())
            .flex()
            .items_stretch()
            .invisible()
            .group_hover("sidebar-row", |this| this.visible())
            .when(show_snooze, |this| {
                this.child(hover_action(SharedString::from(format!("snooze-{}", thread.id)), p).child(Icon::Clock.sized(px(12.))).on_click(cx.listener(
                    move |this, event: &gpui::ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(key) = &snooze_key { this.menu.update(cx, |menu, cx| menu.open_scoped_snooze(key, event.position(), window, cx)); }
                        else { this.open_snooze_menu(&snooze_id, event.position(), window, cx); }
                    },
                )))
            })
            .child(hover_action(SharedString::from(format!("settle-{}", thread.id)), p).mr(px(-4.)).child(Icon::Check.sized(px(14.))).child("Settle").on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.state.update(cx, |state, cx| match &settle_key {
                        Some(key) => state.run_scoped_thread_action(key, t3_core::thread_actions::ThreadAction::Settle, &[], cx),
                        None => state.settle_thread(&settle_id, cx),
                    });
                },
            )));
        let meta = thread.branch.clone().map(|branch| {
            div()
                .mt(px(2.))
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .text_xs()
                .text_color(hsla(p.sidebar_secondary_label))
                .child(Icon::GitBranch.sized(px(12.)))
                .child(div().flex_1().min_w_0().truncate().child(branch))
        });
        let has_meta = meta.is_some();
        div()
            .px(px(8.))
            .py(px(2.))
            .pb(px(3.))
            .child(
                self.row_surface(key, thread, project.clone(), section, active, state.recede && matches!(state.status, ThreadStatus::Preparing | ThreadStatus::Working), cx).child(
                    div()
                        .when(has_meta, |this| this.h(px(CARD_CONTENT_HEIGHT)))
                        .px(px(10.))
                        .py(px(8.))
                        .child(
                            div()
                                .h(px(20.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .min_w_0()
                                .child(self.favicon(&thread.project_id, false, cx))
                                .children(self.pin_indicator(key, thread, cx))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(hsla(p.sidebar_secondary_label))
                                        .children(project),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .h_full()
                                        .min_w(px(32.))
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .text_xs()
                                        .when_some(self.jump_hint(key, thread, cx), |this, hint| this.child(hint))
                                        .when(!self.jump_hints_visible(), |this| {
                                            this.child(div().when(!state.is_woke, |this| this.group_hover("sidebar-row", |this| this.invisible())).child(status_slot))
                                                .child(actions)
                                        }),
                                ),
                        )
                        .child(div().mt(px(4.)).flex().items_center().gap(px(6.)).min_w_0().child(self.render_title(key, thread, title_color, state.recede, cx)))
                        .children(meta),
                ),
            )
            .into_any_element()
    }

    fn render_slim(&mut self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, project: Option<String>, section: Section, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let now = self.now;
        let state = row_state(thread, active, now);
        let title_color = if state.recede {
            hsla(p.secondary_label)
        } else if active || state.is_woke || matches!(state.status, ThreadStatus::Input | ThreadStatus::Request) {
            hsla(p.foreground)
        } else if state.unread {
            hsla(p.muted_foreground)
        } else {
            hsla(p.secondary_label).opacity(0.7)
        };
        let time = match section {
            Section::Snoozed => thread.snoozed_until.map(|until| {
                div().flex().items_center().gap(px(4.)).text_xs().text_color(hsla(p.info_foreground)).child(Icon::AlarmClock.sized(px(12.))).child(snooze_wake_label(until, now)).into_any_element()
            }),
            _ if state.is_woke => {
                let id = thread.id.clone();
                let woke_at = state.woke_at;
                Some(
                    div()
                        .id(SharedString::from(format!("woke-{}", thread.id)))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.warning_foreground))
                        .child(Icon::AlarmClock.sized(px(12.)))
                        .child("Woke")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(woke_at) = woke_at {
                                this.state.update(cx, |state, cx| state.acknowledge_woke(&id, woke_at, cx));
                            }
                        }))
                        .into_any_element(),
                )
            }
            Section::Settled => settled_timestamp(thread).map(|at| div().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(compact_relative_label(at, now)).into_any_element()),
            _ => thread.latest_user_message_at.or(thread.updated_at).map(|at| div().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(compact_relative_label(at, now)).into_any_element()),
        };
        let id = thread.id.clone();
        let action_key = key.cloned();
        let action = match section {
            Section::Snoozed => hover_action(SharedString::from(format!("wake-{}", thread.id)), p).child(Icon::AlarmClockOff.sized(px(12.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| match &action_key { Some(key) => state.run_scoped_thread_action(key, t3_core::thread_actions::ThreadAction::Unsnooze, &[], cx), None => state.unsnooze_thread(&id, cx) });
            })),
            Section::Settled => hover_action(SharedString::from(format!("unsettle-{}", thread.id)), p).child(Icon::Undo2.sized(px(14.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| match &action_key { Some(key) => state.run_scoped_thread_action(key, t3_core::thread_actions::ThreadAction::Unsettle, &[], cx), None => state.unsettle_thread(&id, cx) });
            })),
            _ => hover_action(SharedString::from(format!("settle-{}", thread.id)), p).child(Icon::Check.sized(px(12.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| match &action_key { Some(key) => state.run_scoped_thread_action(key, t3_core::thread_actions::ThreadAction::Settle, &[], cx), None => state.settle_thread(&id, cx) });
            })),
        };
        div()
            .px(px(8.))
            .pb(px(1.))
            .child(
                self.row_surface(key, thread, project, section, active, false, cx).h(px(SLIM_HEIGHT)).child(
                    div()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .child(self.scoped_favicon(key, &thread.project_id, !active && section != Section::Settled, cx))
                        .child(self.render_title(key, thread, title_color, state.recede, cx))
                        .children(self.pin_indicator(key, thread, cx))
                        .child(
                            div()
                                .relative()
                                .h(px(24.))
                                .min_w(px(32.))
                                .flex()
                                .items_center()
                                .justify_end()
                                .when_some(self.jump_hint(key, thread, cx), |this, hint| this.child(hint))
                                .when(!self.jump_hints_visible(), |this| {
                                    this.child(div().when(!state.is_woke, |this| this.group_hover("sidebar-row", |this| this.invisible())).children(time))
                                        .child(div().absolute().right(px(-4.)).top_0().bottom_0().flex().invisible().group_hover("sidebar-row", |this| this.visible()).child(action))
                                }),
                        ),
                ),
            )
            .into_any_element()
    }

    fn scoped_favicon(&self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, project_id: &str, muted: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(key) = key else { return self.favicon(project_id, muted, cx) };
        let visual = crate::project_icon::ProjectVisual::for_scoped_project(self.state.read(cx), &t3_core::sidebar::grouping::ProjectRef { environment_id: key.environment_id.clone(), project_id: project_id.into() });
        let size = crate::project_icon::size(16., cx); let ink = hsla(palette(cx).sidebar_icon);
        if muted { visual.render_receding(size, ink, cx) } else { visual.render(size, ink, cx) }
    }

    /// Slim rows mute the same project visual until hovered.
    pub(super) fn favicon(&self, project_id: &str, muted: bool, cx: &mut Context<Self>) -> AnyElement {
        let size = crate::project_icon::size(16., cx);
        let visual = crate::project_icon::ProjectVisual::for_project(self.state.read(cx), project_id);
        let ink = hsla(palette(cx).sidebar_icon);
        if muted { visual.render_receding(size, ink, cx) } else { visual.render(size, ink, cx) }
    }

    fn jump_hints_visible(&self) -> bool {
        self.jump_hints
    }

    /// The `mod+N` badge on the first nine rows while the jump modifier is held (`JumpHintBadge`).
    fn jump_hint(&self, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, thread: &SidebarThread, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.jump_hints {
            return None;
        }
        let state = self.state.read(cx);
        let position = if let Some(key) = key { state.sidebar.fleet.ordered.iter().take(9).position(|index| state.sidebar.fleet.threads[*index].key == *key)? }
            else { let model = &state.sidebar.model; model.ordered.iter().take(9).position(|index| model.threads[*index].id == thread.id)? };
        let label = crate::keymap::label(t3_core::keybindings::thread_jump_command_for_index(position)?, cx)?;
        let p = palette(cx);
        Some(
            div()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(hsla(p.foreground).opacity(0.07))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(label)
                .into_any_element(),
        )
    }

    fn render_shelf(&self, shelf: Shelf, count: usize, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let tone = if shelf == Shelf::Settled && self.drop_target() == Some(Section::Settled) {
            hsla(p.primary)
        } else if self.drag.is_some() {
            hsla(p.sidebar_foreground)
        } else {
            hsla(p.sidebar_muted_foreground)
        };
        div()
            .px(px(10.))
            .pb(px(1.))
            .child(
                div()
                    .id(SharedString::from(format!("shelf-{}", shelf.label())))
                    .h(px(SHELF_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .hover(|this| this.text_color(hsla(p.sidebar_foreground)))
                    .child(shelf.label())
                    .child(div().flex_1())
                    .child(div().font_weight(FontWeight::NORMAL).child(count.to_string()))
                    .child(if expanded { Icon::ChevronDown.sized(px(12.)) } else { Icon::ChevronRight.sized(px(12.)) })
                    .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |state, cx| state.toggle_shelf(shelf, cx)))),
            )
            .into_any_element()
    }

    fn render_show_more(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        div()
            .px(px(8.))
            .child(
                div()
                    .id("settled-show-more")
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_sm()
                    .text_color(hsla(p.sidebar_muted_foreground))
                    .hover(|this| this.bg(hsla(p.sidebar_row_hover)).text_color(hsla(p.sidebar_foreground)))
                    .child(Icon::ChevronDown.sized(px(16.)))
                    .child(self.state.read(cx).settled_show_more_label(count))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |state, cx| if !state.settled_shelf_loading() { state.show_more_settled(cx) })
                    })),
            )
            .into_any_element()
    }
}

/// Built when the tooltip opens, so rows pay nothing for it while drawing.
pub(super) trait AppearanceExt {
    fn window_appearance_is_dark(&self) -> bool;
}

impl AppearanceExt for App {
    fn window_appearance_is_dark(&self) -> bool {
        gpui_component::ActiveTheme::theme(self).is_dark()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod capture {
    use super::*;
    use gpui::{AppContext, Entity, HeadlessAppContext, Render, size};
    use std::sync::Arc;
    use t3_core::sidebar::thread::{SubagentRequest, present_thread};

    struct Rows {
        pane: Entity<SidebarPane>,
        runtime: &'static str,
    }

    impl Render for Rows {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = palette(cx);
            let mut columns = div().flex().size_full().bg(hsla(p.sidebar));
            for active in [false, true] {
                let mut column = div().w(px(400.)).flex_none().flex().flex_col().child(
                    div().p(px(12.)).text_color(hsla(p.foreground)).child(if active { "Active row" } else { "Background row" }),
                );
                for (index, request) in [None, Some(SubagentRequest::Approval), Some(SubagentRequest::UserInput)].into_iter().enumerate() {
                    let shell = serde_json::from_value(serde_json::json!({
                        "id":format!("capture-{active}-{index}"),"projectId":"fixture-project","title":match request {
                            None => "Goal active", Some(SubagentRequest::Approval) => "Subagent approval", Some(SubagentRequest::UserInput) => "Subagent input"
                        },"createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-01T00:00:00Z",
                        "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                        "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":"capture"},
                        "status":self.runtime,"latestRunId":"capture-run","activeRunId":"capture-run",
                        "activityRunStartedAt":"2026-06-01T00:00:00Z","goal":{"status":"active"},
                        "hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
                    })).unwrap();
                    let mut thread = present_thread(&shell, request);
                    for variant in 0..if request.is_none() { 4 } else { 1 } {
                        thread.id = format!("capture-{active}-{index}-{variant}");
                        thread.has_pending_approvals = variant == 1;
                        thread.has_pending_user_input = variant == 2;
                        thread.pending_guest_message_count = u64::from(variant == 3);
                        thread.title = match variant { 1 => "Approval", 2 => "User input", 3 => "Guest request", _ => shell.title.as_str() }.into();
                        let card = self.pane.update(cx, |pane, cx| {
                            pane.now = 1_780_272_045_000;
                            let card = pane.render_card(None, &thread, Some("Fixture".into()), Section::Active, active, cx);
                            pane.clock = None;
                            card
                        });
                        column = column.child(card);
                    }
                    if request.is_none() {
                        thread.has_pending_approvals = false;
                        thread.has_pending_user_input = false;
                        thread.pending_guest_message_count = 0;
                        thread.title = "Slim settled row".into();
                        let slim = self.pane.update(cx, |pane, cx| pane.render_slim(None, &thread, Some("Fixture".into()), Section::Settled, active, cx));
                        column = column.child(slim);
                    }
                }
                columns = columns.child(column);
            }
            columns
        }
    }

    #[test]
    fn production_status_rows() {
        let Some(out) = std::env::var_os("T3_SIDEBAR_STATUS_CAPTURE") else { return };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(), Arc::new(crate::icons::Assets), gpui_platform::current_headless_renderer,
        );
        let state = cx.update(|cx| {
            let fixture: crate::chatgpt_setup::Fixture = serde_json::from_value(serde_json::json!({
                "provider":{"instanceId":"replay","driver":"replay","enabled":true,"installed":true,"status":"ready"},
                "auth":{"phase":"idle"},"install":{"phase":"idle"}
            })).unwrap();
            cx.set_global(fixture);
            crate::boot(runtime.handle().clone(), cx)
        });
        assert!(state.read_with(&cx, |state, _| state.selected_thread.is_none() && state.client_handle().is_none()));
        let mut rows = None;
        let window = cx.open_window(size(px(800.), px(650.)), |window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu, dialog, snooze, window, cx));
            let view = cx.new(|_| Rows { pane, runtime: "preparing" });
            rows = Some(view.clone());
            view
        }).unwrap();
        let rows = rows.unwrap();
        for (step, status) in ["preparing", "running", "idle", "completed", "preparing"].into_iter().enumerate() {
            rows.update(&mut cx, |rows, cx| { rows.runtime = status; cx.notify(); });
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| { window.draw(cx).clear(); }).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(std::path::Path::new(&out).join(format!("{step}-{status}.png"))).unwrap();
        }
    }

    struct WorkingRows(Entity<SidebarPane>);

    impl Render for WorkingRows {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let count = self.0.read(cx).state.read(cx).sidebar.model.rows.len();
            let mut column = div().size_full().flex().flex_col().bg(hsla(palette(cx).sidebar));
            for index in 0..count {
                column = column.child(self.0.update(cx, |pane, cx| {
                    pane.now = 1_780_272_045_000;
                    let row = pane.render_row(index, window, cx);
                    pane.clock = None;
                    row
                }));
            }
            column
        }
    }

    #[test]
    fn production_cached_working_rows() {
        let Some(out) = std::env::var_os("T3_SIDEBAR_WORKING_CAPTURE") else { return };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(), Arc::new(crate::icons::Assets), gpui_platform::current_headless_renderer,
        );
        let state = cx.update(|cx| {
            cx.set_global(serde_json::from_value::<crate::chatgpt_setup::Fixture>(serde_json::json!({
                "provider":{"instanceId":"replay","driver":"replay","enabled":true,"installed":true,"status":"ready"},
                "auth":{"phase":"idle"},"install":{"phase":"idle"}
            })).unwrap());
            crate::boot(runtime.handle().clone(), cx)
        });
        state.update(&mut cx, |state, _| {
            state.stop_pump();
            let threads: Vec<_> = [("pinned", "Pinned cached thread", None), ("active", "Inbox thread", None),
                ("busy-older", "Working: earlier send", Some("2026-06-01T00:00:00Z")),
                ("busy-newer", "Working: later send", Some("2026-06-01T00:00:30Z"))].into_iter().map(|(id, title, sent)| {
                serde_json::json!({
                    "id":id,"projectId":"fixture-project","title":title,"createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-01T00:00:00Z",
                    "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                    "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
                    "status":if sent.is_some() {"running"} else {"idle"},"latestRunId":sent.map(|_| "fixture-run"),
                    "activeRunId":sent.map(|_| "fixture-run"),"activityRunStartedAt":sent,
                    "latestUserAuthoredMessageAt":sent,"pinnedAt":if id == "pinned" {Some("2026-06-01T00:00:00Z")} else {None},
                    "hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
                })
            }).collect();
            state.shell.snapshot = Some(serde_json::from_value(serde_json::json!({
                "snapshotSequence":1,"projects":[{"id":"fixture-project","title":"Fixture","workspaceRoot":"/fixture",
                    "createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-01T00:00:00Z"}],"threads":threads
            })).unwrap());
            state.sidebar.shelves.working_expanded = true;
        });
        let window = cx.open_window(size(px(400.), px(500.)), |window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu, dialog, snooze, window, cx));
            cx.new(|_| WorkingRows(pane))
        }).unwrap();
        let mut records = Vec::new();
        for (step, (label, connected, enabled)) in [
            ("connected", true, true), ("disconnected", false, true), ("reconnected", true, true),
            ("disabled-disconnected", false, false), ("reenabled-disconnected", false, true),
        ].into_iter().enumerate() {
            state.update(&mut cx, |state, cx| {
                state.phase = crate::app_state::Phase::Online(if connected {
                    t3_client::supervisor::ConnectionStatus::Connected
                } else {
                    t3_client::supervisor::ConnectionStatus::Reconnecting { reason: "Synthetic disconnect".into(), attempt: 1 }
                });
                state.sidebar.settings.working_shelf_enabled = enabled;
                state.rebuild_sidebar(cx);
                assert!(state.selected_thread.is_none() && state.client_handle().is_none() && state.sidebar.drag.is_none());
                records.push(serde_json::json!({"step":label,"connected":connected,"enabled":enabled,
                    "rows":format!("{:?}",state.sidebar.model.rows),"partition":format!("{:?}",state.sidebar.model.partition)}));
            });
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| { window.draw(cx).clear(); }).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(std::path::Path::new(&out).join(format!("{step}-{label}.png"))).unwrap();
        }
        std::fs::write(std::path::Path::new(&out).join("model.json"), serde_json::to_vec_pretty(&records).unwrap()).unwrap();
    }
}

#[cfg(all(test, feature = "snapshot"))]
impl SidebarPane {
    pub(crate) fn render_snooze_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let count = self.state.read(cx).sidebar.model.rows.len();
        let mut rows = div().size_full().flex().flex_col().bg(hsla(palette(cx).sidebar));
        for index in 0..count {
            rows = rows.child(self.render_row(index, window, cx));
        }
        rows.into_any_element()
    }
}
