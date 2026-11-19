//! The app's top-right toast stack (design-system §6.6, `components/ui/toast.tsx`): each toast is
//! its own entry with its own timer and close orb. Collapsed, the newest shows and up to two more
//! peek beneath it; hovering expands the stack and holds every timer.

use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::project_search::{OPEN_IN_EDITOR, open_in_editor_request, preferred_editor};

use crate::app_state::{AppEvent, AppState, Phase};
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
    pub open_thread: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    /// Renders an inline `"Open keybindings.json"` action (`KeybindingsConfigWarning.tsx`).
    pub open_keybindings_config: bool,
    /// Optional status-specific leading icon override (`ThreadNotificationCoordinator.tsx`).
    pub leading_icon: Option<Icon>,
    /// Suppresses the error copy button (e.g. thread notification toasts).
    pub hide_copy_button: bool,
    /// Optional stable key for in-place upsert/close (`SlowRpcRequestToastCoordinator`, etc.).
    pub key: Option<String>,
    /// Legacy-permission re-pair notice (`PermissionUpdateNotice.ts`), carrying the environment
    /// dismissal key so dismissing via Close/Dismiss/Open persists across restarts.
    pub permission_update_env: Option<String>,
    persistent: bool,
    github_star: bool,
    replace_linked_prompt: Option<crate::composer::prompt_links::ReplaceLinkedPrompt>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<SharedString>) -> Toast {
        Toast {
            kind,
            title: title.into(),
            description: None,
            open_thread: None,
            open_keybindings_config: false,
            leading_icon: None,
            hide_copy_button: false,
            key: None,
            permission_update_env: None,
            persistent: false,
            github_star: false,
            replace_linked_prompt: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Toast {
        self.description = Some(description.into());
        self
    }

    pub fn open_environment_thread(mut self, target: t3_core::sidebar::tag_filter::ThreadKey) -> Toast {
        self.open_thread = Some(target);
        self
    }

    pub fn open_keybindings_config(mut self) -> Toast {
        self.open_keybindings_config = true;
        self
    }

    pub fn leading_icon(mut self, icon: Icon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    pub fn hide_copy_button(mut self) -> Self {
        self.hide_copy_button = true;
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn permission_update(mut self, env_key: impl Into<String>) -> Self {
        let env_key = env_key.into();
        let key = format!("permission-update:{env_key}");
        self.permission_update_env = Some(env_key);
        self.key(key).persistent()
    }

    pub fn persistent(mut self) -> Self {
        self.persistent = true;
        self
    }
    pub fn replace_linked_prompt(mut self, replacement: crate::composer::prompt_links::ReplaceLinkedPrompt) -> Self {
        self.replace_linked_prompt = Some(replacement);
        self.persistent()
    }

    pub(crate) fn github_star(mut self) -> Self {
        self.github_star = true;
        self.persistent()
    }

    fn copy_text(&self) -> String {
        match &self.description {
            Some(desc) if !desc.is_empty() => format!("{}\n{}", self.title, desc),
            _ => self.title.to_string(),
        }
    }

    fn render_content(&self, id: u64, copied: bool, state: &Entity<AppState>, cx: &App) -> AnyElement {
        let p = palette(cx);
        let (icon, color) = if let Some(custom) = self.leading_icon {
            let color = match (self.kind, custom) {
                (ToastKind::Success, _) => p.success,
                (ToastKind::Error, _) => p.destructive,
                (_, Icon::MessageCircleQuestion) => p.info,
                (ToastKind::Warning, _) => p.warning,
                (ToastKind::Info, _) => p.info,
            };
            (custom, color)
        } else if self.github_star && self.kind != ToastKind::Success {
            (Icon::Star, p.warning_foreground)
        } else {
            match self.kind {
                ToastKind::Success => (Icon::CircleCheck, p.success),
                ToastKind::Error => (Icon::CircleAlert, p.destructive),
                ToastKind::Warning => (Icon::TriangleAlert, p.warning),
                ToastKind::Info => (Icon::Info, p.info),
            }
        };
        let show_copy = self.kind == ToastKind::Error && !self.hide_copy_button;
        let copy_payload = show_copy.then(|| self.copy_text());
        let copy_state = state.clone();
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
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(self.title.clone()))
                            .when_some(copy_payload, |this, payload| {
                                let label = if copied { "Copied error" } else { "Copy error" };
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("toast-copy-error-{id}")))
                                        .flex_none()
                                        .size(px(20.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(5.))
                                        .cursor_pointer()
                                        .text_color(if copied { hsla(p.success) } else { hsla(p.muted_foreground).opacity(0.8) })
                                        .hover(|s| s.text_color(hsla(p.muted_foreground)))
                                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx))
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(payload.clone()));
                                            copy_state.update(cx, |state, cx| state.mark_toast_copied(id, cx));
                                        })
                                        .child((if copied { Icon::Check } else { Icon::Copy }).sized(px(12.))),
                                )
                            }),
                    )
                    .children(self.description.clone().map(|text| {
                        div()
                            .text_color(hsla(p.muted_foreground))
                            .when(self.kind == ToastKind::Error && text.len() >= 180, |d| d.max_h(px(84.)).overflow_hidden())
                            .child(text)
                    }))
                    .children(self.open_thread.as_ref().map(|_| div().pt(px(4.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.primary)).child("Open thread"))),
            )
            .into_any_element()
    }
}

struct Entry {
    id: u64,
    toast: Toast,
    copied: bool,
    _copy_reset: Option<Task<()>>,
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
    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn latest(&self) -> Option<&Toast> {
        self.entries.first().map(|entry| &entry.toast)
    }

    pub(crate) fn has_thread_notifications(&self) -> bool {
        self.entries.iter().any(|entry| entry.toast.open_thread.is_some())
    }

    pub(crate) fn retire_environment_notifications(&mut self, visible: impl Fn(&t3_core::sidebar::tag_filter::ThreadKey) -> bool) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| entry.toast.open_thread.as_ref().is_none_or(&visible));
        if self.entries.is_empty() { self.hovered = false; }
        count != self.entries.len()
    }
}

impl AppState {
    pub fn show_toast(&mut self, toast: Toast, cx: &mut Context<Self>) {
        if let Some(key) = toast.key.as_deref()
            && let Some(entry) = self.toasts.entries.iter_mut().find(|e| e.toast.key.as_deref() == Some(key))
        {
            let id = entry.id;
            entry._timer = (!toast.persistent).then(|| toast_timer(id, cx));
            entry.toast = toast;
            cx.emit(AppEvent::Notice);
            return;
        }
        self.toasts.next_id += 1;
        let id = self.toasts.next_id;
        self.toasts.entries.insert(
            0,
            Entry {
                id,
                copied: false,
                _copy_reset: None,
                _timer: (!toast.persistent).then(|| toast_timer(id, cx)),
                toast,
            },
        );
        cx.emit(AppEvent::Notice);
    }

    pub fn close_toast_by_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self
            .toasts
            .entries
            .iter()
            .filter(|e| e.toast.key.as_deref() == Some(key))
            .map(|e| e.id)
            .collect();
        for id in ids {
            self.dismiss_toast(id, cx);
        }
    }

    pub(crate) fn mark_toast_copied(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(entry) = self.toasts.entries.iter_mut().find(|entry| entry.id == id) {
            entry.copied = true;
            entry._copy_reset = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1200)).await;
                let _ = this.update(cx, |state, cx| {
                    if let Some(entry) = state.toasts.entries.iter_mut().find(|e| e.id == id) {
                        entry.copied = false;
                        entry._copy_reset = None;
                        cx.emit(AppEvent::Notice);
                    }
                });
            }));
            cx.emit(AppEvent::Notice);
        }
    }

    pub fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.toasts.entries.iter().any(|entry| entry.id == id && entry.toast.github_star) {
            self.close_star_prompt(cx);
        }
        if let Some(env_key) = self
            .toasts
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.toast.permission_update_env.clone())
        {
            let ui_key = format!("permission-update-dismissed-{env_key}");
            t3_client::store::save_ui_state(&ui_key, &serde_json::Value::Bool(true));
        }
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

    pub(crate) fn remove_star_toast(&mut self, cx: &mut Context<Self>) {
        let before = self.toasts.entries.len();
        self.toasts.entries.retain(|entry| !entry.toast.github_star);
        if self.toasts.entries.is_empty() {
            self.toasts.hovered = false;
        }
        if self.toasts.entries.len() != before {
            cx.emit(AppEvent::Notice);
        }
    }
    pub(crate) fn thank_star_toast(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .toasts
            .entries
            .iter_mut()
            .find(|entry| entry.toast.github_star)
        {
            entry.toast.kind = ToastKind::Success;
            entry.toast.title = "Thanks for the star!".into();
            entry.toast.description = Some("It really helps T3 Code grow.".into());
            let id = entry.id;
            entry._timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let _ = this.update(cx, |state, cx| state.expire_toast(id, cx));
            }));
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

    /// `KeybindingsConfigWarning`: opening `keybindings.json` requires an online connection with
    /// `orchestration:operate`, a non-empty `keybindingsConfigPath`, and an available editor.
    pub(crate) fn keybindings_config_open_target(&self) -> Option<(String, &'static str)> {
        if !matches!(self.phase, Phase::Online(ConnectionStatus::Connected)) {
            return None;
        }
        let allowed = self
            .client_handle()
            .and_then(|client| client.session_state().borrow().clone())
            .is_none_or(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE));
        if !allowed {
            return None;
        }
        let config = self.server_config.config.as_ref()?;
        let path = config.keybindings_config_path.clone().filter(|path| !path.is_empty())?;
        preferred_editor(&config.available_editors, None).map(|(editor, _)| (path, editor))
    }

    pub(crate) fn can_open_keybindings_config(&self) -> bool {
        self.keybindings_config_open_target().is_some()
    }

    /// Opens the server's `keybindings.json` in the preferred editor (`KeybindingsConfigWarning`).
    /// On RPC failure, shows an error toast matching the web warning action.
    pub(crate) fn open_keybindings_config(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((path, editor)) = self.keybindings_config_open_target() else {
            return false;
        };
        let task = self.rpc(OPEN_IN_EDITOR, open_in_editor_request(&path, editor));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |state, cx| {
                    state.show_toast(
                        Toast::new(ToastKind::Error, "Unable to open keybindings file").description(error.to_string()),
                        cx,
                    );
                });
            }
        })
        .detach();
        true
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

fn render_toast(id: u64, toast_data: &Toast, copied: bool, state: &Entity<AppState>, window: &mut Window, cx: &mut App) -> AnyElement {
    let toast = surface(cx)
        .w_full()
        .id(SharedString::from(format!("toast-{id}")))
        .child(toast_data.render_content(id, copied, state, cx));
    let toast = if toast_data.github_star {
        toast.children(crate::github_star_prompt::actions(id, state, window, cx))
    } else { toast };
    let toast = if toast_data.permission_update_env.is_some() {
        let p = palette(cx);
        let dismiss_state = state.clone();
        let open_state = state.clone();
        toast.child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .px(px(14.))
                .pb(px(12.))
                .child(
                    div()
                        .id(SharedString::from(format!("toast-perm-dismiss-{id}")))
                        .h(px(24.))
                        .px(px(7.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .text_xs()
                        .text_color(hsla(p.foreground))
                        .cursor_pointer()
                        .hover(|this| this.bg(hsla(p.accent)))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            dismiss_state.update(cx, |state, cx| state.dismiss_toast(id, cx));
                        })
                        .child("Dismiss"),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("toast-perm-open-access-{id}")))
                        .h(px(24.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(hsla(p.primary))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.primary_foreground))
                        .cursor_pointer()
                        .hover(|this| this.opacity(0.9))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            open_state.update(cx, |state, cx| state.dismiss_toast(id, cx));
                            crate::settings::open(t3_core::settings::search::Section::Access, cx);
                        })
                        .child("Open Sharing & access"),
                ),
        )
    } else {
        toast
    };
    let toast = if toast_data.open_keybindings_config {
        let p = palette(cx);
        let can_open = state.read(cx).can_open_keybindings_config();
        let open_state = state.clone();
        toast.child(
            div().px(px(14.)).pb(px(12.)).flex().justify_end().child(
                div()
                    .id(SharedString::from(format!("toast-open-keybindings-{id}")))
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.background))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .when(can_open, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent)))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                open_state.update(cx, |state, cx| {
                                    state.open_keybindings_config(cx);
                                });
                            })
                    })
                    .when(!can_open, |this| this.opacity(0.5))
                    .child("Open keybindings.json"),
            ),
        )
    } else {
        toast
    };
    let toast = if let Some(action) = toast_data.replace_linked_prompt.clone() {
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
    let toast = match toast_data.open_thread.clone() {
        Some(target) => {
            let state = state.clone();
            toast
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    state.update(cx, |state, cx| {
                        state.dismiss_toast(id, cx);
                        state.open_notification_target(target.clone(), cx);
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
pub fn stack(state: &Entity<AppState>, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let (visible, hovered) = {
        let toasts = &state.read(cx).toasts;
        (
            toasts
                .entries
                .iter()
                .take(VISIBLE_LIMIT)
                .map(|entry| (entry.id, entry.toast.clone(), entry.copied))
                .collect::<Vec<_>>(),
            toasts.hovered,
        )
    };
    let front = visible.first()?;
    let expanded = hovered || visible.len() == 1;
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
        container.children(visible.iter().map(|(id, toast, copied)| render_toast(*id, toast, *copied, state, window, cx)))
    } else {
        // Back toasts first so the front one paints over them; each peeks 12px lower, 10% narrower.
        container
            .children((1..=peeks).rev().map(|index| {
                let inset = WIDTH * 0.05 * index as f32;
                surface(cx).absolute().left(px(inset)).right(px(inset)).bottom(px(GAP * (peeks - index) as f32)).h(px(GAP * 2.)).into_any_element()
            }))
            .child(render_toast(front.0, &front.1, front.2, state, window, cx))
    };
    Some(gpui::deferred(container).with_priority(60).into_any_element())
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{Render, size};
    use serde_json::json;

    #[gpui::test]
    async fn keybindings_toast_gates_open_action_and_handles_rpc_errors(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        struct Root(Entity<AppState>);
        impl Render for Root {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().children(stack(&self.0, window, cx))
            }
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });

        // 1. Offline -> disabled even with configPath + editors + operate scope.
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({"authenticated": true, "permissions": ["orchestration:read", "orchestration:operate"]}))
                    .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, _| {
            state.phase = Phase::Unpaired;
            if let Some(config) = state.server_config.config.as_mut() {
                config.keybindings_config_path = Some("/tmp/keybindings.json".to_owned());
                config.available_editors = vec!["vscode".to_owned()];
            }
        });
        assert!(!state.read_with(cx, |state, _| state.can_open_keybindings_config()));

        // 2. Online without `orchestration:operate` -> disabled.
        fixture
            .grants
            .send(Some(serde_json::from_value(json!({"authenticated": true, "permissions": ["orchestration:read"]})).unwrap()))
            .unwrap();
        state.update(cx, |state, _| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
        });
        assert!(!state.read_with(cx, |state, _| state.can_open_keybindings_config()));

        // 3. Online + `orchestration:operate`, but missing `configPath` -> disabled.
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({"authenticated": true, "permissions": ["orchestration:read", "orchestration:operate"]}))
                    .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, _| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.keybindings_config_path = Some(String::new());
                config.available_editors = vec!["vscode".to_owned()];
            }
        });
        assert!(!state.read_with(cx, |state, _| state.can_open_keybindings_config()));

        // 4. Online + `orchestration:operate` + `configPath`, but empty `availableEditors` -> disabled.
        state.update(cx, |state, _| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.keybindings_config_path = Some("/tmp/keybindings.json".to_owned());
                config.available_editors.clear();
            }
        });
        assert!(!state.read_with(cx, |state, _| state.can_open_keybindings_config()));

        // 5. All gates met -> enabled and resolves target.
        state.update(cx, |state, _| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.available_editors = vec!["vscode".to_owned()];
            }
        });
        assert_eq!(
            state.read_with(cx, |state, _| state.keybindings_config_open_target()),
            Some(("/tmp/keybindings.json".to_owned(), "vscode"))
        );

        // Render warning toast with inline action button in both enabled & disabled states.
        state.update(cx, |state, cx| {
            state.show_toast(
                Toast::new(ToastKind::Warning, "Invalid keybindings configuration")
                    .description("Bad entry at index 0")
                    .open_keybindings_config(),
                cx,
            );
        });
        let window = cx.open_window(size(px(800.), px(600.)), |_, _| Root(state.clone()));
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        // Triggering open sends `shell.openInEditor` and surfaces an error toast on failure.
        let (err_tx, err_rx) = tokio::sync::oneshot::channel::<()>();
        let mut err_tx = Some(err_tx);
        let _notice_sub = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event: &AppEvent, _| {
                if *event == AppEvent::Notice
                    && state.toasts.latest().is_some_and(|t| t.kind == ToastKind::Error)
                    && let Some(tx) = err_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        let started = state.update(cx, |state, cx| state.open_keybindings_config(cx));
        assert!(started);
        let (method, payload, reply) = loop {
            let (method, payload, reply) = fixture.next_request().await;
            if method == OPEN_IN_EDITOR {
                break (method, payload, reply);
            }
            let _ = reply.send(Ok(json!({})));
        };
        assert_eq!(method, OPEN_IN_EDITOR);
        assert_eq!(payload["cwd"], "/tmp/keybindings.json");
        assert_eq!(payload["editor"], "vscode");
        reply
            .send(Err(t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure {
                tag: None,
                message: "editor binary not found".into(),
                error: serde_json::Value::Null,
            })))
            .unwrap();
        err_rx.await.unwrap();
        drop(_notice_sub);
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let latest = state.toasts.latest().expect("error toast on open failure");
            assert_eq!(latest.kind, ToastKind::Error);
            assert_eq!(latest.title, "Unable to open keybindings file");
            assert!(latest.description.as_deref().is_some_and(|d| d.contains("editor binary not found")));
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn keyed_upsert_and_permission_update_toast_persist_dismissal(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        struct Root(Entity<AppState>);
        impl Render for Root {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().children(stack(&self.0, window, cx))
            }
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });

        // Keyed toast upserts in place instead of duplicating.
        state.update(cx, |state, cx| {
            state.show_toast(Toast::new(ToastKind::Warning, "Slow 1").key("slow-rpc").persistent(), cx);
            state.show_toast(Toast::new(ToastKind::Warning, "Slow 2").key("slow-rpc").persistent(), cx);
        });
        state.read_with(cx, |state, _| {
            assert_eq!(state.toasts.entries.len(), 1);
            assert_eq!(state.toasts.latest().unwrap().title, "Slow 2");
        });
        state.update(cx, |state, cx| state.close_toast_by_key("slow-rpc", cx));
        state.read_with(cx, |state, _| assert!(state.toasts.entries.is_empty()));

        // Permission-update toast persists dismissal on dismiss and renders cleanly alongside an error toast.
        let env_key = "test-legacy-env-dismiss";
        state.update(cx, |state, cx| {
            state.show_toast(
                Toast::new(ToastKind::Warning, "Permissions have changed for Local")
                    .description("Pair again.")
                    .permission_update(env_key),
                cx,
            );
            state.show_toast(
                Toast::new(ToastKind::Error, "Failed to sync").description("Network timeout"),
                cx,
            );
        });
        let window = cx.open_window(size(px(800.), px(600.)), |_, _| Root(state.clone()));
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        state.update(cx, |state, cx| state.close_toast_by_key(&format!("permission-update:{env_key}"), cx));
        // Flush the single-threaded UI-state writer deterministically before reading back.
        let _ = t3_client::store::github_star_prompt(t3_client::store::StarPromptStorage::Read).await;
        assert_eq!(
            t3_client::store::load_ui_state(&format!("permission-update-dismissed-{env_key}")).and_then(|v| v.as_bool()),
            Some(true)
        );

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
