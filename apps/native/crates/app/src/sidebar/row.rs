//! Row rendering (design-system §7.1–7.3, `sidebar/SidebarThreadRow.tsx`,
//! `sidebar/SidebarListMarkers.tsx`): 78px cards for pinned, active and working threads, 36px slim
//! rows for snoozed and settled ones, shelf headers, and the Settled "Show more" button.

use gpui::{AnyElement, App, BoxShadow, Context, FontWeight, Hsla, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, point, prelude::*, px};
use t3_core::sidebar::settled::{self, snooze_wake_label};
use t3_core::sidebar::status::{self, StatusIcon, StatusTone, ThreadStatus, TopStatus};
use t3_core::sidebar::thread::SidebarThread;
use t3_core::sidebar::time::{compact_relative_label, working_duration_label};
use t3_core::sidebar::{Section, Shelf, SidebarRow, sort::settled_timestamp};

use super::SidebarPane;
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

fn tone_color(tone: StatusTone, p: &Palette) -> u32 {
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
fn hover_action(id: SharedString, p: &Palette) -> gpui::Stateful<gpui::Div> {
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
        let model = self.state.read(cx).sidebar.model.clone();
        let Some(row) = model.rows.get(index) else { return div().into_any_element() };
        match row {
            SidebarRow::Thread { index: thread_index, section } => {
                let thread = &model.threads[*thread_index];
                let project = model.project_names.get(&thread.project_id).cloned();
                let active = self.state.read(cx).selected_thread.as_deref() == Some(thread.id.as_str());
                if section.is_card() {
                    self.render_card(thread, project, active, cx)
                } else {
                    self.render_slim(thread, *section, active, cx)
                }
            }
            SidebarRow::Shelf { shelf, count, expanded } => self.render_shelf(*shelf, *count, *expanded, cx),
            SidebarRow::ShowMore { count } => self.render_show_more(*count, cx),
        }
    }

    fn row_surface(&self, thread: &SidebarThread, active: bool, recede_working: bool, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = palette(cx);
        let light = !cx.window_appearance_is_dark();
        let id = thread.id.clone();
        let menu_id = thread.id.clone();
        div()
            .id(SharedString::from(format!("thread-row-{}", thread.id)))
            .group("sidebar-row")
            .relative()
            .w_full()
            .rounded(px(6.))
            .cursor_pointer()
            .when(active, |this| {
                this.bg(hsla(p.sidebar_row_active)).when(light, |this| {
                    this.shadow(vec![BoxShadow { color: hsla(p.sidebar_border).opacity(0.7), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: true }])
                })
            })
            .when(!active, |this| this.hover(|this| this.bg(hsla(p.sidebar_row_hover))))
            .when(recede_working, |this| this.opacity(0.7).hover(|this| this.opacity(1.)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.click_count() > 1 {
                    this.start_rename(&id, window, cx);
                    return;
                }
                this.state.update(cx, |state, cx| state.select_thread(&id, cx));
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                this.open_menu(&menu_id, event.position, window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_title(&self, thread: &SidebarThread, color: Hsla, recede: bool, cx: &mut Context<Self>) -> AnyElement {
        if let Some((_, input)) = self.rename.as_ref().filter(|(id, _)| *id == thread.id) {
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
                .h(px(20.))
                .px(px(4.))
                .rounded(px(4.))
                .border_1()
                .border_color(hsla(p.input))
                .bg(hsla(p.card))
                .text_sm()
                .child(gpui_component::input::Input::new(input).appearance(false).bordered(false))
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

    fn pin_indicator(&self, thread: &SidebarThread, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !thread.pinned {
            return None;
        }
        let p = palette(cx);
        let id = thread.id.clone();
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
                    this.state.update(cx, |state, cx| state.unpin_thread(&id, cx));
                }))
                .into_any_element(),
        )
    }

    fn render_card(&mut self, thread: &SidebarThread, project: Option<String>, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let now = self.now;
        let state = row_state(thread, active, now);
        let show_snooze = settled::can_snooze(thread, now);
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
                    ThreadStatus::Working => status::working_started_at(thread),
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
                        this.open_snooze_menu(&snooze_id, event.position(), window, cx);
                    },
                )))
            })
            .child(hover_action(SharedString::from(format!("settle-{}", thread.id)), p).mr(px(-4.)).child(Icon::Check.sized(px(14.))).child("Settle").on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.state.update(cx, |state, cx| state.settle_thread(&settle_id, cx));
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
                self.row_surface(thread, active, state.recede && state.status == ThreadStatus::Working, cx).child(
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
                                .children(self.pin_indicator(thread, cx))
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
                                        .when_some(self.jump_hint(thread, cx), |this, hint| this.child(hint))
                                        .when(!self.jump_hints_visible(), |this| {
                                            this.child(div().when(!state.is_woke, |this| this.group_hover("sidebar-row", |this| this.invisible())).child(status_slot))
                                                .child(actions)
                                        }),
                                ),
                        )
                        .child(div().mt(px(4.)).flex().items_center().gap(px(6.)).min_w_0().child(self.render_title(thread, title_color, state.recede, cx)))
                        .children(meta),
                ),
            )
            .into_any_element()
    }

    fn render_slim(&mut self, thread: &SidebarThread, section: Section, active: bool, cx: &mut Context<Self>) -> AnyElement {
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
        let action = match section {
            Section::Snoozed => hover_action(SharedString::from(format!("wake-{}", thread.id)), p).child(Icon::AlarmClockOff.sized(px(12.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| state.unsnooze_thread(&id, cx));
            })),
            Section::Settled => hover_action(SharedString::from(format!("unsettle-{}", thread.id)), p).child(Icon::Undo2.sized(px(14.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| state.unsettle_thread(&id, cx));
            })),
            _ => hover_action(SharedString::from(format!("settle-{}", thread.id)), p).child(Icon::Check.sized(px(12.))).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| state.settle_thread(&id, cx));
            })),
        };
        div()
            .px(px(8.))
            .pb(px(1.))
            .child(
                self.row_surface(thread, active, false, cx).h(px(SLIM_HEIGHT)).child(
                    div()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .child(self.render_title(thread, title_color, state.recede, cx))
                        .children(self.pin_indicator(thread, cx))
                        .child(
                            div()
                                .relative()
                                .h(px(24.))
                                .min_w(px(32.))
                                .flex()
                                .items_center()
                                .justify_end()
                                .when_some(self.jump_hint(thread, cx), |this, hint| this.child(hint))
                                .when(!self.jump_hints_visible(), |this| {
                                    this.child(div().when(!state.is_woke, |this| this.group_hover("sidebar-row", |this| this.invisible())).children(time))
                                        .child(div().absolute().right(px(-4.)).top_0().bottom_0().flex().invisible().group_hover("sidebar-row", |this| this.visible()).child(action))
                                }),
                        ),
                ),
            )
            .into_any_element()
    }

    fn jump_hints_visible(&self) -> bool {
        self.jump_hints
    }

    /// The `mod+N` badge on the first nine rows while the jump modifier is held (`JumpHintBadge`).
    fn jump_hint(&self, thread: &SidebarThread, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.jump_hints {
            return None;
        }
        let model = self.state.read(cx).sidebar.model.clone();
        let position = model.ordered.iter().take(9).position(|index| model.threads[*index].id == thread.id)?;
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
                    .text_color(hsla(p.sidebar_muted_foreground))
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

pub(super) trait AppearanceExt {
    fn window_appearance_is_dark(&self) -> bool;
}

impl AppearanceExt for App {
    fn window_appearance_is_dark(&self) -> bool {
        gpui_component::ActiveTheme::theme(self).is_dark()
    }
}
