//! Guest links and promotion: `GuestShareControls.tsx`, `guestShareAvailability.ts`,
//! `useThreadActions.ts` and the existing `guestShare.create`/`projects.createFromThread` RPCs.
use std::collections::HashSet;

use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::{
    app_state::{AppEvent, AppState},
    context_menu::ThreadMenuOwner,
    icons::Icon,
    sidebar::controller::now_ms,
    theme::{hsla, palette},
    toast::{Toast, ToastKind},
};
use crate::theme::slots::{
    ThemeSlot, ThemeSlotExt, slot_border_color, slot_has_background,
    theme_slot_clipped_background_layer,
};
use gpui::{
    AnyElement, App, AppContext, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, Styled, Subscription, Task, WeakEntity,
    Window, deferred, div, point, prelude::*, px, relative,
};
use gpui_component::input::{Escape, Input, InputState};
use gpui_component::{h_flex, v_flex};
use t3_core::thread_actions::{ThreadAction, guest_share as rules};

impl AppState {
    pub(crate) fn can_share_scoped(&self, owner: &ThreadMenuOwner) -> bool {
        let Some(destination) = &owner.destination else { return self.can_share_with_guest(); };
        let Some(environment) = self.scoped_environment(&destination.saved_id) else { return false; };
        rules::available(environment.config.config.as_ref(), self.scoped_ready(destination, t3_core::auth::ACCESS_WRITE_SCOPE))
    }

    pub(crate) fn can_promote_scoped(&self, owner: &ThreadMenuOwner) -> bool {
        let Some(key) = owner.key() else { return self.can_turn_into_project(&owner.thread); };
        let Some(environment) = self.scoped_environment(&key.environment_id) else { return false; };
        environment.config.config.as_ref().is_some_and(|config| config.environment.capabilities.thread_project_from_scratch == Some(true))
            && self.scoped_thread(&key).is_some_and(|row| row.thread.worktree_path.as_ref().is_some_and(|path| !path.is_empty()))
            && owner.project_ref().and_then(|key| self.scoped_project(&key)).is_some_and(|project| project.is_scratch)
    }

    pub(crate) fn promote_scoped(&mut self, owner: ThreadMenuOwner, cx: &mut Context<Self>) {
        if !owner.allows(self, &ThreadAction::TurnIntoProject) { return; }
        let Some(key) = owner.key() else { return; };
        let Some(row) = self.scoped_thread(&key).filter(|row| row.thread.can_archive()) else { return; };
        let title = row.thread.title.clone();
        let destination = owner.destination.clone().unwrap();
        let target = self.connections.destination(&destination, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE);
        cx.spawn(async move |this, cx| {
            let Ok(target) = target.await else { return; };
            if !this.read_with(cx, |state, _| target.is_current(&state.fleet) && owner.allows(state, &ThreadAction::TurnIntoProject)
                && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))).unwrap_or(false) { return; }
            let response = target.client.request(t3_core::projects::PROJECTS_CREATE_FROM_THREAD, t3_core::projects::create_from_thread(&owner.thread)).await;
            if response.as_ref().err().is_some_and(interrupted) { return; }
            let result = response.map_err(|error| error.to_string()).and_then(|value| serde_json::from_value::<t3_core::projects::CreateFromThreadResult>(value).map_err(|error| error.to_string()));
            let _ = this.update(cx, |state, cx| {
                if !owner.response_current(state) || !owner.can_operate_owner(state) { return; }
                let toast = match result {
                    Ok(result) if !result.project_id.is_empty() => Toast::new(ToastKind::Success, "Project created").description(title),
                    Ok(_) => Toast::new(ToastKind::Error, "Could not turn thread into a project").description("The server returned no project."),
                    Err(error) => Toast::new(ToastKind::Error, "Could not turn thread into a project").description(error),
                };
                state.show_toast(toast, cx);
            });
        }).detach();
    }

    pub(crate) fn can_share_with_guest(&self) -> bool {
        rules::available(
            self.server_config.config.as_ref(),
            self.client_handle().is_some_and(|client| {
                client
                    .session_state()
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| session.grants(t3_core::auth::ACCESS_WRITE_SCOPE))
            }),
        )
    }
    pub(crate) fn can_turn_into_project(&self, id: &str) -> bool {
        self.server_config.config.as_ref().is_some_and(|config| {
            config.environment.capabilities.thread_project_from_scratch == Some(true)
        }) && self.thread_shell(id).is_some_and(|thread| {
            thread
                .worktree_path
                .as_ref()
                .is_some_and(|path| !path.is_empty())
                && self.is_scratch_project(&thread.project_id)
        })
    }
    pub(crate) fn turn_into_project(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let owner = ThreadMenuOwner::capture(self, id)?;
        if !owner.allows(self, &ThreadAction::TurnIntoProject)
            || self
                .sidebar
                .model
                .thread(id)
                .is_none_or(|thread| !thread.can_archive())
        {
            return None;
        }
        let title = self.thread_shell(id).unwrap().title.clone();
        let request = self.rpc(
            t3_core::projects::PROJECTS_CREATE_FROM_THREAD,
            t3_core::projects::create_from_thread(id),
        );
        Some(cx.spawn(async move |this, cx| {
            let response = request.await;
            if response.as_ref().err().is_some_and(interrupted) {
                return;
            }
            let result = response
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<t3_core::projects::CreateFromThreadResult>(value)
                        .map_err(|error| error.to_string())
                });
            let _ = this.update(cx, |state, cx| {
                if !owner.response_current(state) || !ThreadMenuOwner::can_operate(state) {
                    return;
                }
                let toast = match result {
                    Ok(result) if !result.project_id.is_empty() => {
                        Toast::new(ToastKind::Success, "Project created").description(title)
                    }
                    Ok(_) => Toast::new(ToastKind::Error, "Could not turn thread into a project")
                        .description("The server returned no project."),
                    Err(error) => {
                        Toast::new(ToastKind::Error, "Could not turn thread into a project")
                            .description(error.to_string())
                    }
                };
                state.show_toast(toast, cx);
            });
        }))
    }
    pub(crate) fn guest_share(&self, id: &str) -> Option<&rules::GuestShare> {
        self.thread_state(id)
            .projection
            .as_ref()
            .filter(|projection| projection.thread.id == id)?
            .thread
            .guest_share
            .as_ref()
    }

    // The transcript merge adds the tail row and empty-state checks.
    #[allow(dead_code)]
    pub(crate) fn has_pending_guest_messages(&self, id: &str) -> bool {
        self.guest_share(id).is_some_and(|share| share.pending_messages().next().is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PendingMessageKey {
    share: String,
    message: String,
}

#[derive(Default)]
struct GuestMessageRequests(HashSet<PendingMessageKey>);
impl Global for GuestMessageRequests {}

// The transcript merge installs this in the virtualized tail row.
#[allow(dead_code)]
pub(crate) fn render_pending_guest_messages(
    thread_id: Option<&str>,
    app: &Entity<AppState>,
    cx: &mut App,
) -> Option<AnyElement> {
    let thread_id = thread_id?;
    let state = app.read(cx);
    let share = state.guest_share(thread_id)?;
    share.pending_messages().next()?;
    let owner = ThreadMenuOwner::capture(state, thread_id).filter(|owner| owner.can_operate_owner(state));
    let scope = state.thread_copy_scope();
    let media_scope = cx.try_global::<crate::media::Media>().and_then(|media| media.scope.as_ref());
    let requests = cx.try_global::<GuestMessageRequests>();
    let guest = share.guest_name.as_deref().unwrap_or_else(|| crate::i18n::t(cx, "Guest"));
    let p = palette(cx);
    Some(v_flex()
        .w_full()
        .gap(px(16.))
        .pb(px(16.))
        .children(share.pending_messages().map(|message| {
            let key = PendingMessageKey { share: share.id.clone(), message: message.id.clone() };
            let busy = requests.is_some_and(|requests| requests.0.contains(&key));
            let attachments = message.attachments.as_deref().unwrap_or_default();
            v_flex()
                .id(gpui::SharedString::from(format!("pending-guest:{}", message.id)))
                .w_full()
                .items_end()
                .gap(px(4.))
                .child(h_flex().gap(px(4.)).text_xs().text_color(hsla(p.guest_foreground))
                    .child(Icon::UserRound.render(px(12.), hsla(p.guest_foreground)))
                    .child(crate::i18n::t_fmt(cx, "{name} (guest)", &[("name", guest.into())])))
                .child(v_flex().min_w_0().max_w(relative(0.8)).gap(px(8.)).rounded(px(16.)).p(px(12.))
                    .bg(hsla(p.guest_surface))
                    .children(attachments.iter().enumerate().map(|(ix, attachment)| {
                        let target = t3_core::right_panel::FileAttachment {
                            id: attachment.id.clone(),
                            name: attachment.name.clone(),
                            mime_type: attachment.mime_type.clone(),
                            size_bytes: attachment.size_bytes,
                            url: None,
                        };
                        let thumb = media_scope.filter(|_| attachment.kind == "image").map(|scope| {
                            let source = t3_core::media::MediaSource::Asset(t3_core::media::AssetResource::Attachment {
                                attachment_id: attachment.id.clone(),
                            });
                            div().h(px(96.)).child(crate::media::scoped_image(&source, scope))
                        });
                        let app = app.clone();
                        let scope = scope.clone();
                        v_flex()
                            .id(("guest-attachment", ix))
                            .min_w_0()
                            .children(thumb)
                            .child(Button::new("open", ButtonVariant::Ghost, ButtonSize::Xs)
                                .icon(Icon::Paperclip)
                                .label(attachment.name.clone())
                                .on_click(move |_, _, cx| {
                                    if scope.is_some() && app.read(cx).thread_copy_scope() == scope {
                                        app.update(cx, |state, cx| state.open_attachment(target.clone(), cx));
                                    }
                                }))
                    }))
                    .when(!message.text.is_empty(), |bubble| bubble.child(
                        div().text_sm().text_color(hsla(p.foreground)).child(message.text.clone()),
                    )))
                .children(owner.as_ref().map(|owner| {
                    h_flex().gap(px(4.)).pr(px(4.)).children([
                        ("dismiss", "Dismiss", ButtonVariant::GhostMuted, rules::GuestMessageDecision::Dismiss),
                        ("send", "Send", ButtonVariant::Default, rules::GuestMessageDecision::Send),
                    ].into_iter().map(|(id, label, variant, decision)| {
                        let app = app.clone();
                        let owner = owner.clone();
                        let key = key.clone();
                        Button::new(id, variant, ButtonSize::Xs)
                            .label(label)
                            .disabled(busy)
                            .on_click(move |_, _, cx| resolve_guest_message(&app, &owner, &key, decision, cx))
                    }))
                }))
        }))
        .into_any_element())
}

fn resolve_guest_message(
    app: &Entity<AppState>,
    owner: &ThreadMenuOwner,
    key: &PendingMessageKey,
    decision: rules::GuestMessageDecision,
    cx: &mut App,
) {
    let state = app.read(cx);
    if !owner.current(state)
        || !owner.can_operate_owner(state)
        || !state.guest_share(&owner.thread).is_some_and(|share| {
            share.id == key.share && share.pending_messages().any(|message| message.id == key.message)
        })
    {
        return;
    }
    let command = rules::resolve_guest_message(&uuid::Uuid::new_v4().to_string(), &owner.thread, &key.message, decision);
    let request = state.rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
    if !cx.has_global::<GuestMessageRequests>() {
        cx.set_global(GuestMessageRequests::default());
    }
    if !cx.global_mut::<GuestMessageRequests>().0.insert(key.clone()) {
        return;
    }
    cx.refresh_windows();
    let owner = owner.clone();
    let key = key.clone();
    app.update(cx, |_, cx| {
        cx.spawn(async move |app, cx| {
            let result = request.await;
            let _ = app.update(cx, |state, cx| {
                cx.global_mut::<GuestMessageRequests>().0.remove(&key);
                cx.refresh_windows();
                if let Err(error) = result
                    && !interrupted(&error)
                    && owner.response_current(state)
                    && state.guest_share(&owner.thread).is_some_and(|share| share.id == key.share)
                {
                    state.show_toast(
                        Toast::new(ToastKind::Error, crate::i18n::t(cx, "Could not resolve guest message").to_owned())
                            .description(error.to_string()),
                        cx,
                    );
                }
            });
        }).detach();
    });
}

fn interrupted(error: &t3_client::session::RpcError) -> bool {
    matches!(error, t3_client::session::RpcError::Failure(failure) if failure.tag.is_none() && failure.error.is_null() && failure.message == "Interrupted")
}

fn create_error(error: t3_client::session::RpcError) -> String {
    if let t3_client::session::RpcError::Failure(failure) = &error
        && failure.tag.as_deref() == Some("GuestShareCreateError")
        && let Some(message) = failure
            .error
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .and_then(rules::failure_message)
    {
        return message.into();
    }
    error.to_string()
}

struct ShareWatch(tokio::task::JoinHandle<()>);
impl Drop for ShareWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Host(WeakEntity<GuestShareDialog>);
impl Global for Host {}
pub(crate) fn open(owner: ThreadMenuOwner, window: gpui::AnyWindowHandle, cx: &mut App) {
    let Some(dialog) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) else {
        return;
    };
    dialog.update(cx, |dialog, cx| {
        if !owner.allows(dialog.state.read(cx), &ThreadAction::ShareGuest) {
            return;
        }
        if dialog
            .pending
            .as_ref()
            .is_some_and(|pending| pending.owner == owner)
        {
            return;
        }
        dialog.task = None;
        dialog.copied_reset = None;
        dialog.active_share = owner.destination.is_none().then(|| dialog.state.read(cx).guest_share(&owner.thread).cloned()).flatten();
        dialog.pending = Some(Pending {
            owner: owner.clone(),
            state: CreateState::Idle,
        });
        dialog.watch_share(owner, cx);
        cx.notify();
    });
    cx.defer(move |cx| {
        let _ = cx.update_window(window, |_, window, cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.previous_focus = window.focused(cx);
                window.focus(&dialog.focus, cx);
            })
        });
    });
}
#[derive(Debug, PartialEq)]
enum CreateState {
    Idle,
    Creating,
    Failed(String),
    Created(String, bool),
}
struct Pending {
    owner: ThreadMenuOwner,
    state: CreateState,
}
pub(crate) struct GuestShareDialog {
    state: Entity<AppState>,
    pending: Option<Pending>,
    focus: FocusHandle,
    link: Entity<InputState>,
    footer_focus: [FocusHandle; 4],
    previous_focus: Option<FocusHandle>,
    task: Option<Task<()>>,
    share_watch: Option<Task<()>>,
    copied_reset: Option<Task<()>>,
    active_share: Option<rules::GuestShare>,
    _subscriptions: Vec<Subscription>,
}
impl GuestShareDialog {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.set_global(Host(cx.weak_entity()));
        let link = cx.new(|cx| InputState::new(window, cx));
        let link_focus = link.read(cx).focus_handle(cx);
        let on_focus = cx.on_focus(&link_focus, window, |this, window, cx| {
            this.link.update(cx, |link, cx| link.select_all(window, cx))
        });
        let subscription =
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if this.pending.is_none()
                    || !matches!(
                        event,
                        AppEvent::Phase | AppEvent::Shell | AppEvent::ServerConfig
                    )
                {
                    return;
                }
                if this
                    .pending
                    .as_ref()
                    .is_some_and(|pending| !pending.owner.current(this.state.read(cx)))
                {
                    if let Some(pending) = this.pending.as_mut() {
                        pending.state = CreateState::Idle;
                    }
                    this.close(window, cx);
                } else if *event != AppEvent::Shell {
                    cx.notify();
                }
            });
        GuestShareDialog {
            state,
            pending: None,
            focus: cx.focus_handle(),
            link,
            footer_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            previous_focus: None,
            task: None,
            share_watch: None,
            copied_reset: None,
            active_share: None,
            _subscriptions: vec![subscription, on_focus],
        }
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.state == CreateState::Creating)
        {
            return;
        }
        self.pending = None;
        self.task = None;
        self.share_watch = None;
        self.copied_reset = None;
        self.active_share = None;
        if self.focus.contains_focused(window, cx)
            && let Some(focus) = self.previous_focus.take()
        {
            window.focus(&focus, cx);
        }
        cx.notify();
    }
    fn watch_share(&mut self, owner: ThreadMenuOwner, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let local_client = state.client_handle();
        let scoped_client = owner.destination.as_ref().map(|destination| state.connections.destination(destination, t3_core::auth::ACCESS_WRITE_SCOPE));
        let thread = owner.thread.clone();
        let (send, mut receive) = tokio::sync::watch::channel(self.active_share.clone());
        let net = ShareWatch(state.runtime().spawn(async move {
            let client = match scoped_client {
                Some(resolve) => match resolve.await { Ok(destination) => destination.client, Err(_) => return },
                None => match local_client { Some(client) => client, None => return },
            };
            let Ok(mut stream) = client
                .stream(
                    t3_core::commands::method::SUBSCRIBE_THREAD,
                    t3_core::commands::subscribe_thread(&thread, None),
                )
                .await
            else {
                return;
            };
            let mut projection = t3_core::thread::ThreadState::default();
            while let Some(Ok(value)) = stream.receiver.recv().await {
                let Ok(item) = t3_core::thread::decode_thread_stream_item(value) else {
                    continue;
                };
                if projection.apply(item) {
                    let share = projection
                        .projection
                        .as_ref()
                        .and_then(|projection| projection.thread.guest_share.clone());
                    send.send_if_modified(|current| {
                        if *current == share {
                            false
                        } else {
                            *current = share;
                            true
                        }
                    });
                }
            }
        }));
        self.share_watch = Some(cx.spawn(async move |this, cx| {
            let _net = net;
            while receive.changed().await.is_ok() {
                let share = receive.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        if this
                            .pending
                            .as_ref()
                            .is_none_or(|pending| pending.owner != owner)
                            || !owner.current(this.state.read(cx))
                        {
                            return false;
                        }
                        this.active_share = share;
                        cx.notify();
                        true
                    })
                    .ok()
                    != Some(true)
                {
                    return;
                }
            }
        }));
    }
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if matches!(
            pending.state,
            CreateState::Creating | CreateState::Created(..)
        ) || !pending
            .owner
            .allows(self.state.read(cx), &ThreadAction::ShareGuest)
        {
            return;
        }
        let owner = pending.owner.clone();
        pending.state = CreateState::Creating;
        let state = self.state.read(cx);
        let local_request = owner.destination.is_none().then(|| state.rpc(rules::CREATE, rules::create(&owner.thread)));
        let destination = owner.destination.as_ref().map(|destination| state.connections.destination(destination, t3_core::auth::ACCESS_WRITE_SCOPE));
        let target_thread = owner.thread.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let response = match destination {
                Some(resolve) => {
                    let Ok(target) = resolve.await else { return; };
                    if !this.read_with(cx,|dialog,cx| target.is_current(&dialog.state.read(cx).fleet) && owner.allows(dialog.state.read(cx),&ThreadAction::ShareGuest)
                        && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ACCESS_WRITE_SCOPE))).unwrap_or(false) { return; }
                    target.client.request(rules::CREATE,rules::create(&target_thread)).await
                }
                None => local_request.expect("local owner").await,
            };
            let was_interrupted = response.as_ref().err().is_some_and(interrupted);
            let result = response.map_err(create_error).and_then(|value| {
                serde_json::from_value::<rules::CreateResult>(value)
                    .map_err(|error| error.to_string())
            });
            let _ = this.update_in(cx, |this, window, cx| {
                if !owner.allows(this.state.read(cx), &ThreadAction::ShareGuest) {
                    if let Some(pending) = this.pending.as_mut() {
                        pending.state = CreateState::Idle;
                    }
                    this.close(window, cx);
                    return;
                }
                let Some(pending) = this.pending.as_mut().filter(|pending| {
                    pending.owner == owner && pending.state == CreateState::Creating
                }) else {
                    return;
                };
                pending.state = if was_interrupted {
                    CreateState::Idle
                } else {
                    match result {
                        Ok(result)
                            if !result.url.trim().is_empty() && !result.share.id.is_empty() =>
                        {
                            this.link.update(cx, |link, cx| {
                                link.set_value(result.url.clone(), window, cx)
                            });
                            CreateState::Created(result.url, false)
                        }
                        Ok(_) => CreateState::Failed("Could not create a guest link.".into()),
                        Err(message) => CreateState::Failed(message),
                    }
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if !pending
            .owner
            .allows(self.state.read(cx), &ThreadAction::ShareGuest)
        {
            return;
        }
        if let CreateState::Created(url, copied) = &mut pending.state {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
            *copied = true;
            let owner = pending.owner.clone();
            let url = url.clone();
            self.copied_reset = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(2_000))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(pending) = this
                        .pending
                        .as_mut()
                        .filter(|pending| pending.owner == owner)
                        && let CreateState::Created(current, copied) = &mut pending.state
                        && *current == url
                    {
                        *copied = false;
                        cx.notify();
                    }
                });
            }));
            cx.notify();
        }
    }
}
impl Render for GuestShareDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(pending) = self.pending.as_ref() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let creating = pending.state == CreateState::Creating;
        let created = matches!(pending.state, CreateState::Created(..));
        let available = pending
            .owner
            .allows(self.state.read(cx), &ThreadAction::ShareGuest);
        let dialog_custom = slot_has_background(ThemeSlot::DialogPopup, cx);
        let dialog_border = slot_border_color(ThemeSlot::DialogPopup, cx).unwrap_or_else(|| hsla(p.dialog_border));
        let panel = match &pending.state {
            CreateState::Created(_, copied) => {
                let input_background = theme_slot_clipped_background_layer(ThemeSlot::Input, px(8.), cx);
                let input_custom = input_background.is_some();
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .children(input_background)
                            .child(Input::new(&self.link).readonly(true).appearance(!input_custom).theme_slot(ThemeSlot::Input, cx)),
                    )
                    .child(
                        Button::new("guest-copy", ButtonVariant::Outline, ButtonSize::Sm)
                            .focus_handle(self.footer_focus[0].clone())
                            .icon(if *copied { Icon::Check } else { Icon::Copy })
                            .label(if *copied { "Copied" } else { "Copy" })
                            .disabled(!available)
                            .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                    )
                    .into_any_element()
            }
            _ => v_flex()
                .gap(px(16.))
                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(
                    if self.active_share.is_some() {
                        "A new link ends the current one."
                    } else {
                        "The link works once and expires in 24 hours."
                    },
                ))
                .children(match &pending.state {
                    CreateState::Failed(message) => Some(
                        div()
                            .text_xs()
                            .text_color(hsla(p.destructive))
                            .child(message.clone()),
                    ),
                    _ => None,
                })
                .into_any_element(),
        };
        deferred(div().id("guest-share-backdrop").absolute().inset_0().occlude().flex().items_center().justify_center().p(px(16.)).bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(v_flex().id("guest-share-dialog").track_focus(&self.focus).w_full().max_w(px(512.)).rounded(px(16.)).relative()
                .when(!dialog_custom, |this| this.child(crate::glass::backdrop(16., hsla(p.dialog_surface), dialog_border, cx)))
                .when(dialog_custom, |this| this.bg(hsla(p.dialog_surface)).border_1().border_color(dialog_border))
                .children(theme_slot_clipped_background_layer(ThemeSlot::DialogPopup, px(16.), cx))
                .theme_slot(ThemeSlot::DialogPopup, cx)
                .shadow(vec![BoxShadow { color:gpui::black().opacity(0.65), offset:point(px(0.), px(24.)), blur_radius:px(64.), spread_radius:px(-24.), inset:false }])
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_action(cx.listener(|this, _: &Escape, window, cx| { this.close(window, cx); cx.stop_propagation(); }))
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key != "tab" { return; }
                    let Some(pending) = &this.pending else { return };
                    let handles = match pending.state {
                        CreateState::Created(..) => {
                            let mut handles = vec![this.link.read(cx).focus_handle(cx)];
                            if pending.owner.allows(this.state.read(cx), &ThreadAction::ShareGuest) { handles.push(this.footer_focus[0].clone()); }
                            handles.push(this.footer_focus[1].clone());
                            handles
                        },
                        CreateState::Creating => vec![this.focus.clone()],
                        _ if pending.owner.allows(this.state.read(cx), &ThreadAction::ShareGuest) => vec![this.footer_focus[2].clone(), this.footer_focus[3].clone()],
                        _ => vec![this.footer_focus[2].clone()],
                    };
                    let current = handles.iter().position(|focus| focus.is_focused(window));
                    let index = if event.keystroke.modifiers.shift { current.map_or(handles.len()-1, |index| (index+handles.len()-1)%handles.len()) } else { current.map_or(0, |index| (index+1)%handles.len()) };
                    window.focus(&handles[index], cx);
                    cx.stop_propagation();
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() { "escape" => this.close(window, cx), "enter" => if this.pending.as_ref().is_some_and(|pending| matches!(pending.state, CreateState::Created(..))) { this.close(window, cx); } else { this.create(window, cx); }, _ => return }
                    cx.stop_propagation();
                }))
                .child(v_flex().p(px(24.)).gap(px(8.)).child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Share with guest"))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("The guest sees the thread as you do, including command output and the agent's reasoning. Each message they send waits for you.")))
                .child(div().px(px(24.)).pt(px(4.)).pb(px(24.)).child(panel))
                .child(h_flex().justify_end().gap(px(8.)).px(px(24.)).py(px(16.)).border_t_1().border_color(hsla(p.border)).rounded_b(px(15.)).bg(hsla(p.muted).opacity(0.72))
                    .when(created, |footer| footer.child(Button::new("guest-done", ButtonVariant::Default, ButtonSize::Sm).focus_handle(self.footer_focus[1].clone()).label("Done").on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))))
                    .when(!created, |footer| footer.child(Button::new("guest-cancel", ButtonVariant::Outline, ButtonSize::Sm).focus_handle(self.footer_focus[2].clone()).label("Cancel").disabled(creating).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))))
                        .child(Button::new("guest-create", ButtonVariant::Default, ButtonSize::Sm).focus_handle(self.footer_focus[3].clone()).label(if creating {"Creating..."} else {"Create link"}).disabled(creating || !available).on_click(cx.listener(|this, _, window, cx| this.create(window, cx))))))))
            .with_priority(50).into_any_element()
    }
}

#[derive(PartialEq)]
struct IndicatorState {
    thread: String,
    share: String,
    expires: String,
    name: Option<String>,
    can_operate: bool,
}

pub(crate) struct GuestIndicator {
    state: Entity<AppState>,
    closing: Option<(ThreadMenuOwner, String)>,
    shown: Option<IndicatorState>,
    expiry: Option<Task<()>>,
    task: Option<Task<()>>,
    _subscription: Subscription,
}
impl GuestIndicator {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(
                event,
                AppEvent::Thread
                    | AppEvent::Selection
                    | AppEvent::Shell
                    | AppEvent::Phase
                    | AppEvent::ServerConfig
            ) && this.sync(cx)
            {
                cx.notify();
            }
        });
        let mut view = Self {
            state,
            closing: None,
            shown: None,
            expiry: None,
            task: None,
            _subscription: subscription,
        };
        view.sync(cx);
        view
    }
    fn sync(&mut self, cx: &mut Context<Self>) -> bool {
        let state = self.state.read(cx);
        let closing_changed = self.closing.as_ref().is_some_and(|(owner, share)| {
            !owner.current(state)
                || state
                    .guest_share(&owner.thread)
                    .is_none_or(|current| current.id != *share)
        });
        if closing_changed {
            self.closing = None;
        }
        let shown = state.selected_thread.as_ref().and_then(|thread| {
            state.guest_share(thread).map(|share| IndicatorState {
                thread: thread.clone(),
                share: share.id.clone(),
                expires: share.expires_at.clone(),
                name: share.guest_name.clone(),
                can_operate: ThreadMenuOwner::can_operate(state),
            })
        });
        if shown == self.shown && !closing_changed {
            return false;
        }
        self.shown = shown;
        self.expiry = None;
        let at = state
            .selected_thread
            .as_deref()
            .and_then(|id| state.guest_share(id))
            .and_then(|share| t3_core::sidebar::time::parse_iso(&share.expires_at))
            .map(|at| at / 60_000 * 60_000);
        if let Some(at) = at.filter(|at| *at > now_ms()) {
            let wait = (at - now_ms()).max(0) as u64;
            self.expiry = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(wait))
                    .await;
                let _ = this.update(cx, |_, cx| cx.notify());
            }));
        }
        true
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if self.closing.is_some() || !ThreadMenuOwner::can_operate(state) {
            return;
        }
        let Some(id) = &state.selected_thread else {
            return;
        };
        let Some(owner) = ThreadMenuOwner::capture(state, id) else {
            return;
        };
        let Some(share) = state.guest_share(id) else {
            return;
        };
        let share = share.id.clone();
        self.closing = Some((owner.clone(), share.clone()));
        let request = state.rpc(
            t3_core::commands::method::DISPATCH_COMMAND,
            rules::close(&uuid::Uuid::new_v4().to_string(), id),
        );
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.closing.as_ref() != Some(&(owner.clone(), share.clone())) {
                    return;
                }
                this.closing = None;
                if owner.current(this.state.read(cx))
                    && ThreadMenuOwner::can_operate(this.state.read(cx))
                    && this
                        .state
                        .read(cx)
                        .guest_share(&owner.thread)
                        .is_some_and(|current| current.id == share)
                    && let Err(error) = result
                    && !interrupted(&error)
                {
                    this.state.update(cx, |state, cx| {
                        state.show_toast(
                            Toast::new(ToastKind::Error, "Failed to stop guest share")
                                .description(error.to_string()),
                            cx,
                        )
                    });
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for GuestIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let Some(share) = state
            .selected_thread
            .as_deref()
            .and_then(|id| state.guest_share(id))
        else {
            return div().into_any_element();
        };
        h_flex()
            .flex_none()
            .gap(px(4.))
            .text_xs()
            .text_color(hsla(palette(cx).muted_foreground))
            .child(Icon::UserRound.sized(px(14.)))
            .child(share.status(now_ms()))
            .child(
                Button::new("guest-stop", ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label("Stop sharing")
                    .disabled(self.closing.is_some() || !ThreadMenuOwner::can_operate(state))
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests;
