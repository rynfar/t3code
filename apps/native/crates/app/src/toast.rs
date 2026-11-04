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
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: SharedString,
    pub description: Option<SharedString>,
    /// Clicking the toast opens this thread ("Open thread" on the web's thread notifications).
    pub open_thread: Option<String>,
    persistent: bool,
    replace_linked_prompt: Option<crate::composer::prompt_links::ReplaceLinkedPrompt>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<SharedString>) -> Toast {
        Toast { kind, title: title.into(), description: None, open_thread: None, persistent: false, replace_linked_prompt: None }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Toast {
        self.description = Some(description.into());
        self
    }

    pub fn open_thread(mut self, thread_id: String) -> Toast {
        self.open_thread = Some(thread_id);
        self
    }

    pub fn persistent(mut self) -> Self {
        self.persistent = true;
        self
    }
    pub fn replace_linked_prompt(mut self, replacement: crate::composer::prompt_links::ReplaceLinkedPrompt) -> Self {
        self.replace_linked_prompt = Some(replacement);
        self.persistent()
    }

    fn render_content(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let (icon, color) = match self.kind {
            ToastKind::Success => (Icon::CircleCheck, p.success),
            ToastKind::Error => (Icon::CircleAlert, p.destructive),
            ToastKind::Warning => (Icon::CircleAlert, p.warning),
            ToastKind::Info => (Icon::Info, p.info),
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
    _timer: Option<Task<()>>,
}

/// Newest first.
#[derive(Default)]
pub struct Toasts {
    next_id: u64,
    entries: Vec<Entry>,
    hovered: bool,
}

impl Toasts {
    pub(crate) fn has_thread_notifications(&self) -> bool {
        self.entries.iter().any(|entry| entry.toast.open_thread.is_some())
    }

    pub(crate) fn retire_thread_notifications(&mut self, visible: impl Fn(&str) -> bool) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| entry.toast.open_thread.as_deref().is_none_or(&visible));
        if self.entries.is_empty() { self.hovered = false; }
        count != self.entries.len()
    }
}

impl AppState {
    pub fn show_toast(&mut self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.next_id += 1;
        let id = self.toasts.next_id;
        self.toasts.entries.insert(0, Entry { id, _timer: (!toast.persistent).then(|| toast_timer(id, cx)), toast });
        cx.emit(AppEvent::Notice);
    }

    pub fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.entries.len();
        self.toasts.entries.retain(|entry| entry.id != id);
        // The stack unmounts with its last toast, so no hover-out will arrive.
        if self.toasts.entries.is_empty() {
            self.toasts.hovered = false;
        }
        if self.toasts.entries.len() != before {
            cx.emit(AppEvent::Notice);
        }
    }

    /// A timer ran out: the toast goes, unless the pointer holds the stack open.
    fn expire_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.toasts.hovered {
            self.dismiss_toast(id, cx);
        } else if let Some(entry) = self.toasts.entries.iter_mut().find(|entry| entry.id == id) {
            entry._timer = Some(toast_timer(id, cx));
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
    let toast = if let Some(action) = entry.toast.replace_linked_prompt.clone() {
        toast.child(div().px(px(14.)).pb(px(12.)).child(
            div().id(SharedString::from(format!("replace-linked-{id}"))).cursor_pointer().text_sm().text_color(hsla(palette(cx).primary)).child("Replace draft").on_click({
                let state = state.clone();
                move |_, window, cx| {
                    state.update(cx, |state, cx| state.dismiss_toast(id, cx));
                    window.dispatch_action(Box::new(action.clone()), cx);
                }
            })
        ))
    } else { toast };
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

/// The stack, positioned by the window: top-right, inset 32, below the 52px header. It paints over
/// the settings modal and dialogs, as toasts sit above them on the web (layer 100).
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
    Some(gpui::deferred(container).with_priority(60).into_any_element())
}
