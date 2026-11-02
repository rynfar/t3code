//! Link pull request (`LinkPullRequestDialog.tsx`): a URL, or a number for the thread's own
//! repository, validated as it is typed and linked with `thread.pull-request.link`.

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Render, Styled, Subscription, Window,
    deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::h_flex;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use t3_client::session::RpcError;
use t3_core::commands::method::DISPATCH_COMMAND;
use t3_core::pull_requests::failure_message;
use t3_core::pull_requests::link::{ChangeRequestLink, LinkProject, link_command, link_validation, resolve_link_input};

use super::controls::{Button, Size, Variant};
use crate::app_state::{AppState, uuid_v4};
use crate::theme::{hsla, palette};

pub struct LinkPullRequestDialog {
    state: Entity<AppState>,
    thread_id: Option<String>,
    project: Option<LinkProject>,
    input: Entity<InputState>,
    dirty: bool,
    pending: bool,
    submit_error: Option<String>,
    _subscription: Subscription,
}

impl Focusable for LinkPullRequestDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl LinkPullRequestDialog {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Pull request URL or #42"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.dirty = true;
                this.submit_error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            _ => {}
        });
        LinkPullRequestDialog { state, thread_id: None, project: None, input, dirty: false, pending: false, submit_error: None, _subscription: subscription }
    }

    /// Opens for the selected thread; nothing to link to without one.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread_id) = state.selected_thread.clone() else { return };
        self.project = state
            .thread_shell(&thread_id)
            .and_then(|thread| state.shell.project(&thread.project_id))
            .and_then(|project| project.repository_identity.as_ref())
            .and_then(LinkProject::from_identity);
        self.thread_id = Some(thread_id);
        self.dirty = false;
        self.pending = false;
        self.submit_error = None;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.pending {
            self.thread_id = None;
            cx.notify();
        }
    }

    fn resolved(&self, cx: &App) -> (String, Option<Result<ChangeRequestLink, String>>) {
        let reference = self.input.read(cx).value().to_string();
        let resolved = resolve_link_input(&reference, self.project.as_ref());
        (reference, resolved)
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        let (_, resolved) = self.resolved(cx);
        let (Some(Ok(link)), Some(thread_id)) = (resolved, self.thread_id.clone()) else { return cx.notify() };
        self.pending = true;
        self.submit_error = None;
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, link_command(&uuid_v4(), &thread_id, &link));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(_) => this.thread_id = None,
                    Err(RpcError::Failure(failure)) => this.submit_error = Some(failure_message(failure.tag.as_deref(), &failure.error, "Could not link the pull request.")),
                    Err(RpcError::Closed) => this.submit_error = Some("Could not link the pull request.".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LinkPullRequestDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.thread_id.is_none() {
            return div().into_any_element();
        }
        let p = palette(cx);
        let (reference, resolved) = self.resolved(cx);
        let validation = if self.dirty { link_validation(&reference, resolved.as_ref()) } else { None };
        let preview = match &resolved {
            Some(Ok(link)) => Some(format!("{}/{} #{}", link.host, link.repository, link.number)),
            _ => None,
        };
        let message = validation.or(self.submit_error.clone());
        let can_link = !self.pending && matches!(resolved, Some(Ok(_)));
        deferred(
            div()
                .id("link-pr-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
                .child(
                    div()
                        .id("link-pr-dialog")
                        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                            this.close(cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(576.))
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                        .child(
                            div()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Link pull request"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child("Attach a pull request to this thread by its URL, or by its number for this thread's repository."),
                                ),
                        )
                        .child(
                            div()
                                .px(px(24.))
                                .pb(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(Input::new(&self.input))
                                .children(preview.map(|preview| div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(preview)))
                                .children(message.map(|message| div().text_xs().text_color(hsla(p.destructive)).child(message))),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(24.))
                                .py(px(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .rounded_b(px(15.))
                                .bg(hsla(p.muted).opacity(0.72))
                                .child(Button::new("link-pr-cancel", Variant::Outline, Size::Sm).label("Cancel").disabled(self.pending).on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                                .child(
                                    Button::new("link-pr-submit", Variant::Default, Size::Sm)
                                        .label(if self.pending { "Linking..." } else { "Link" })
                                        .disabled(!can_link)
                                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                                ),
                        ),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}
