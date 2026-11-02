//! The app's top-right toast stack (design-system §6.6, `components/ui/toast.tsx`): each toast is
//! its own entry with its own timer and close orb. Collapsed, the newest shows and up to two more
//! peek beneath it; hovering expands the stack and holds every timer.

use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, div, hsla as gpui_hsla, point, px,
};

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// Base UI's default `limit`: toasts past it stay queued but hidden.
const VISIBLE_LIMIT: usize = 3;
const TIMEOUT: Duration = Duration::from_secs(4);
const WIDTH: f32 = 360.;
/// Stacked toasts sit 12px apart and peek 12px.
const GAP: f32 = 12.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: SharedString,
    pub description: Option<SharedString>,
    /// Clicking the toast opens this thread ("Open thread" on the web's thread notifications).
    pub open_thread: Option<String>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<SharedString>) -> Toast {
        Toast { kind, title: title.into(), description: None, open_thread: None }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Toast {
        self.description = Some(description.into());
        self
    }

    pub fn open_thread(mut self, thread_id: String) -> Toast {
        self.open_thread = Some(thread_id);
        self
    }

    fn render_content(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let (icon, color) = match self.kind {
            ToastKind::Success => (Icon::CircleCheck, p.success),
            ToastKind::Error => (Icon::CircleAlert, p.destructive),
            ToastKind::Warning => (Icon::CircleAlert, p.warning),
        };
        div()
            .relative()
            .w_full()
            .flex()
            .gap(px(8.))
            .pl(px(14.))
            .pr(px(16.))
            .py(px(12.))
            .text_sm()
            .text_color(hsla(p.popover_foreground))
            .child(div().pt(px(2.)).child(icon.sized(px(16.)).text_color(hsla(color))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.title.clone()))
                    .children(self.description.clone().map(|text| div().text_color(hsla(p.muted_foreground)).child(text)))
                    .children(self.open_thread.as_ref().map(|_| div().pt(px(4.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.primary)).child("Open thread"))),
            )
            .into_any_element()
    }
}

struct Entry {
    id: u64,
    toast: Toast,
    _timer: Task<()>,
}

/// Newest first.
#[derive(Default)]
pub struct Toasts {
    next_id: u64,
    entries: Vec<Entry>,
    hovered: bool,
}

impl AppState {
    pub fn show_toast(&mut self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.next_id += 1;
        let id = self.toasts.next_id;
        self.toasts.entries.insert(0, Entry { id, toast, _timer: toast_timer(id, cx) });
        cx.emit(AppEvent::Notice);
    }

    pub fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.entries.len();
        self.toasts.entries.retain(|entry| entry.id != id);
        if self.toasts.entries.len() != before {
            cx.emit(AppEvent::Notice);
        }
    }

    /// A timer ran out: the toast goes, unless the pointer holds the stack open.
    fn expire_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.toasts.hovered {
            self.dismiss_toast(id, cx);
        } else if let Some(entry) = self.toasts.entries.iter_mut().find(|entry| entry.id == id) {
            entry._timer = toast_timer(id, cx);
        }
    }

    fn set_toasts_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.toasts.hovered != hovered {
            self.toasts.hovered = hovered;
            cx.emit(AppEvent::Notice);
        }
    }
}

fn toast_timer(id: u64, cx: &mut Context<AppState>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(TIMEOUT).await;
        let _ = this.update(cx, |state, cx| state.expire_toast(id, cx));
    })
}

/// A toast's surface: dropdown glass, radius 8, the toast shadow.
fn surface(cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .relative()
        .rounded(px(8.))
        .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
        .shadow(vec![
            BoxShadow { color: gpui_hsla(0., 0., 0., 0.25), offset: point(px(0.), px(20.)), blur_radius: px(25.), spread_radius: px(-5.), inset: false },
            BoxShadow { color: gpui_hsla(0., 0., 0., 0.25), offset: point(px(0.), px(8.)), blur_radius: px(10.), spread_radius: px(-6.), inset: false },
        ])
}

/// The close orb on a toast's corner: 24px, `border` at 60%, `popover` at 92%, the pill shadow.
fn close_orb(id: u64, state: Entity<AppState>, cx: &App) -> AnyElement {
    let p = palette(cx);
    div()
        .absolute()
        .top(px(-6.))
        .right(px(-6.))
        .child(
            div()
                .id(SharedString::from(format!("toast-close-{id}")))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(hsla(p.border).opacity(0.6))
                .bg(hsla(p.popover).opacity(0.92))
                .text_color(hsla(p.muted_foreground))
                .shadow(vec![
                    BoxShadow { color: gpui_hsla(0., 0., 0., 0.1), offset: point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.), inset: false },
                    BoxShadow { color: gpui_hsla(0., 0., 0., 0.1), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(-1.), inset: false },
                ])
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.popover)).text_color(hsla(p.foreground)))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    state.update(cx, |state, cx| state.dismiss_toast(id, cx));
                })
                .child(Icon::X.sized(px(12.))),
        )
        .into_any_element()
}

fn render_toast(entry: &Entry, state: &Entity<AppState>, cx: &App) -> AnyElement {
    let id = entry.id;
    let toast = surface(cx).w_full().id(SharedString::from(format!("toast-{id}"))).child(entry.toast.render_content(cx));
    let toast = match entry.toast.open_thread.clone() {
        Some(thread_id) => {
            let state = state.clone();
            toast
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    state.update(cx, |state, cx| {
                        state.dismiss_toast(id, cx);
                        state.select_thread(&thread_id, cx);
                    })
                })
                .into_any_element()
        }
        None => toast.into_any_element(),
    };
    div().relative().w_full().child(toast).child(close_orb(id, state.clone(), cx)).into_any_element()
}

/// The stack, positioned by the window: top-right, inset 32, below the 52px header.
pub fn stack(state: &Entity<AppState>, cx: &App) -> Option<AnyElement> {
    let toasts = &state.read(cx).toasts;
    let visible: Vec<&Entry> = toasts.entries.iter().take(VISIBLE_LIMIT).collect();
    let front = visible.first()?;
    let expanded = toasts.hovered || visible.len() == 1;
    let peeks = if expanded { 0 } else { visible.len() - 1 };
    let hover_state = state.clone();
    let container = div()
        .id("toast-stack")
        .absolute()
        .top(px(32. + 52.))
        .right(px(32.))
        .w(px(WIDTH))
        .flex()
        .flex_col()
        .gap(px(GAP))
        .pb(px(GAP * peeks as f32))
        .on_hover(move |hovered, _, cx| hover_state.update(cx, |state, cx| state.set_toasts_hovered(*hovered, cx)));
    let container = if expanded {
        container.children(visible.iter().map(|entry| render_toast(entry, state, cx)))
    } else {
        // Back toasts first so the front one paints over them; each peeks 12px lower, 10% narrower.
        container
            .children((1..=peeks).rev().map(|index| {
                let inset = WIDTH * 0.05 * index as f32;
                surface(cx).absolute().left(px(inset)).right(px(inset)).bottom(px(GAP * (peeks - index) as f32)).h(px(GAP * 2.)).into_any_element()
            }))
            .child(render_toast(front, state, cx))
    };
    Some(container.into_any_element())
}
