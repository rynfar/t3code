//! "Edit from here" (`useTimelineRewind.ts`, `RevertConfirmHost` in `ChatOverlayHosts.tsx`):
//! which user messages can rewind, the confirmation, attachment restore download before rollback,
//! the `checkpoint.rollback` dispatch, the wait for the projection to show it landed, and the
//! prompt/attachments handed back to the composer.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, BoxShadow, Context, FocusHandle, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, SharedString, Styled, Task, Window, anchored,
    deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use serde_json::Value;
use t3_client::supervisor::{ClientHandle, ConnectionStatus};
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::composer::attachments::{AttachmentKind, MAX_ATTACHMENTS, MAX_FILE_BYTES};
use t3_core::media::{AssetResource, CREATE_ASSET_URL, decode_asset_url, resolve_asset_url};
use t3_core::thread::Visibility;
use t3_core::timeline::{
    CONFIRM_DESCRIPTION, CONFIRM_TITLE, EntryKind, INTERRUPT_FIRST, KEEP_CHANGES, NO_ROLLBACK, REVERT_FILES, REWIND_FAILED, REWIND_TIMED_OUT, REWIND_TIMEOUT_SECS,
    RevertInput, RewindProgress, RewindWatch, available_revert_turn_counts, find_revert_checkpoint, recallable_prompt, revert_turn_counts, rewind_progress,
    rollback_command, unsettled_run_id, watch_rewind,
};
use t3_core::turn_item::TurnItemKind;

use super::ThreadView;
use crate::app_state::{AppState, Phase};
use crate::theme::{hsla, palette};

const PENDING_ATTACHMENTS: &str = "Wait for attachments to finish preparing before rewinding.";
const MAX_ATTACHMENTS_REACHED: &str = "Make room for this message's attachments in the composer before rewinding.";
const UNSUPPORTED_ATTACHMENT: &str = "This message has an attachment that cannot be restored.";
const INVALID_ATTACHMENT_URL: &str = "The environment returned an invalid attachment URL.";

use crate::app_state::RestoredAttachment;

#[derive(Clone)]
struct AttachmentSpec {
    kind: AttachmentKind,
    id: String,
    name: String,
    mime_type: String,
}

struct Confirm {
    message_id: String,
    turn_count: u64,
    text: String,
    restore_focus: Option<FocusHandle>,
}

struct Active {
    watch: RewindWatch,
    text: String,
    attachments: Vec<RestoredAttachment>,
    accepted: bool,
    _timeout: Task<()>,
}

#[derive(Default)]
pub(super) struct RewindUi {
    /// The turn count each user message rewinds to, for the messages that can.
    pub counts: HashMap<String, u64>,
    confirm: Option<Confirm>,
    preparing: bool,
    prepare_generation: u64,
    active: Option<Active>,
    focus: Option<FocusHandle>,
}

impl RewindUi {
    pub fn busy(&self) -> bool {
        self.preparing || self.active.is_some()
    }
}

impl ThreadView {
    fn supports_rollback(&self, state: &AppState) -> bool {
        let Some(projection) = self.thread_state(state).projection.as_ref() else {
            return false;
        };
        let rollback = state.server_config.config.as_ref().and_then(|config| {
            config
                .providers
                .iter()
                .find(|provider| provider.instance_id == projection.thread.provider_instance_id)
                .map(|provider| provider.supports_conversation_rollback)
        });
        rollback.is_some() && rollback.flatten() != Some(false)
    }

    fn rewind_guard_error(&self, message_id: &str, cx: &gpui::App) -> Result<(String, ClientHandle), Option<String>> {
        let state = self.state.read(cx);
        let Some(projection) = self.thread_state(state).projection.as_ref() else {
            return Err(None);
        };
        let client = state.client_handle();
        if client
            .as_ref()
            .and_then(|c| c.session_state().borrow().as_ref().map(|session| !session.grants(ORCHESTRATION_OPERATE_SCOPE)))
            .unwrap_or(false)
        {
            return Err(None);
        }
        if !self.supports_rollback(state) {
            return Err(Some(NO_ROLLBACK.to_owned()));
        }
        let Some(client) = client.filter(|_| state.phase == Phase::Online(ConnectionStatus::Connected)) else {
            let label = state
                .active_environment()
                .map(|env| env.label.as_str())
                .filter(|label| !label.is_empty())
                .unwrap_or("this environment");
            return Err(Some(format!("Reconnect {label} before reverting checkpoints.")));
        };
        if !self.rewind.counts.contains_key(message_id) {
            return Err(Some(NO_ROLLBACK.to_owned()));
        }
        let thread_id = projection.thread.id.clone();
        let precheck = crate::composer::rewind_precheck(&thread_id, cx);
        if self.activity.is_working || precheck.send_busy {
            return Err(Some(INTERRUPT_FIRST.to_owned()));
        }
        Ok((thread_id, client))
    }

    fn message_attachment_specs(&self, message_id: &str) -> Result<Vec<AttachmentSpec>, String> {
        let raw_attachments = self
            .entries_cache
            .entries()
            .iter()
            .find_map(|entry| match &entry.kind {
                EntryKind::Message(message) if message.id == message_id => match message.item.as_ref().map(|item| &item.kind) {
                    Some(TurnItemKind::UserMessage { attachments, .. }) => Some(attachments.as_slice()),
                    _ => Some(&[][..]),
                },
                _ => None,
            })
            .unwrap_or(&[]);
        parse_attachment_specs(raw_attachments)
    }

    /// `deriveRevertTurnCountByUserMessageId` with the web's inputs. While a turn runs the
    /// buttons are disabled anyway, so the counts are left as they were rather than re-derived
    /// on every streamed token.
    pub(super) fn sync_revert_counts(&mut self, cx: &mut Context<Self>) {
        if self.activity.is_working && !self.rewind.counts.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let Some(projection) = self.thread_state(state).projection.as_ref() else {
            self.rewind.counts.clear();
            return;
        };
        if !self.supports_rollback(state) {
            self.rewind.counts.clear();
            return;
        }
        let available = available_revert_turn_counts(projection);
        let inherited: HashSet<String> =
            projection.visible_turn_items.iter().filter(|row| row.visibility == Visibility::Inherited).map(|row| row.item.id.clone()).collect();
        let unsettled = unsettled_run_id(&self.activity);
        let input = RevertInput {
            entries: self.entries_cache.entries(),
            checkpoints: &projection.checkpoints,
            thread_start_ready: !self.thread_state(state).history.has_more_history && find_revert_checkpoint(&projection.checkpoints, 0).is_some(),
            available: &available,
            unsettled_run_id: unsettled.as_deref(),
            inherited: &inherited,
        };
        self.rewind.counts = revert_turn_counts(&input);
    }

    /// `onRevertToTurnCount` before the confirmation: the guards, then the dialog.
    pub fn request_rewind(&mut self, message_id: &str, turn_count: u64, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewind.busy() {
            return;
        }
        if let Err(error) = self.rewind_guard_error(message_id, cx) {
            if let Some(message) = error {
                self.show_local_error(message, cx);
            }
            return;
        }
        let focus = self.rewind.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let restore_focus = window.focused(cx);
        self.rewind.confirm = Some(Confirm { message_id: message_id.to_owned(), turn_count, text: text.to_owned(), restore_focus });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn cancel_rewind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.rewind.confirm.take().and_then(|confirm| confirm.restore_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// `rewindToMessage`: validate composer/attachments, download message attachments, dispatch
    /// the rollback, then wait for the projection to show it.
    fn confirm_rewind(&mut self, restore_files: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Confirm { message_id, turn_count, text, restore_focus }) = self.rewind.confirm.take() else { return };
        if let Some(focus) = restore_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
        if self.rewind.busy() {
            return;
        }
        let (thread_id, client) = match self.rewind_guard_error(&message_id, cx) {
            Ok(ready) => ready,
            Err(error) => {
                if let Some(message) = error {
                    self.show_local_error(message, cx);
                }
                return;
            }
        };
        let precheck = crate::composer::rewind_precheck(&thread_id, cx);
        if precheck.pending_attachments {
            self.show_local_error(PENDING_ATTACHMENTS.to_owned(), cx);
            return;
        }
        let specs = match self.message_attachment_specs(&message_id) {
            Ok(specs) => specs,
            Err(message) => {
                self.show_local_error(message, cx);
                return;
            }
        };
        if precheck.staged_count + specs.len() > MAX_ATTACHMENTS {
            self.show_local_error(MAX_ATTACHMENTS_REACHED.to_owned(), cx);
            return;
        }
        if specs.is_empty() {
            self.start_rewind_dispatch(&message_id, turn_count, text, restore_files, Vec::new(), cx);
            return;
        }
        self.rewind.preparing = true;
        self.rewind.prepare_generation = self.rewind.prepare_generation.wrapping_add(1);
        let generation = self.rewind.prepare_generation;
        self.local_error = None;
        cx.notify();
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move { prepare_reverted_attachments(&client, &specs).await });
        cx.spawn(async move |this, cx| {
            let fetched = task.await.unwrap_or_else(|_| Err(REWIND_FAILED.to_owned()));
            let _ = this.update(cx, |view, cx| {
                if view.rewind.prepare_generation != generation {
                    return;
                }
                view.rewind.preparing = false;
                if view.thread_id.as_deref() != Some(thread_id.as_str()) {
                    return;
                }
                let attachments = match fetched {
                    Ok(attachments) => attachments,
                    Err(message) => {
                        view.show_local_error(message, cx);
                        cx.notify();
                        return;
                    }
                };
                if let Err(error) = view.rewind_guard_error(&message_id, cx) {
                    if let Some(message) = error {
                        view.show_local_error(message, cx);
                    }
                    cx.notify();
                    return;
                }
                let precheck = crate::composer::rewind_precheck(&thread_id, cx);
                if precheck.pending_attachments {
                    view.show_local_error(PENDING_ATTACHMENTS.to_owned(), cx);
                    cx.notify();
                    return;
                }
                if precheck.staged_count + attachments.len() > MAX_ATTACHMENTS {
                    view.show_local_error(MAX_ATTACHMENTS_REACHED.to_owned(), cx);
                    cx.notify();
                    return;
                }
                view.start_rewind_dispatch(&message_id, turn_count, text, restore_files, attachments, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn start_rewind_dispatch(
        &mut self,
        message_id: &str,
        turn_count: u64,
        text: String,
        restore_files: bool,
        attachments: Vec<RestoredAttachment>,
        cx: &mut Context<Self>,
    ) {
        if !self.rewind.counts.contains_key(message_id) {
            return;
        }
        let state = self.state.read(cx);
        let Some(projection) = self.thread_state(state).projection.as_ref() else { return };
        let Some(checkpoint) = find_revert_checkpoint(&projection.checkpoints, turn_count) else {
            self.show_local_error(REWIND_FAILED.to_owned(), cx);
            return;
        };
        let command_id = crate::app_state::uuid_v4();
        let watch = match watch_rewind(projection, turn_count, &command_id) {
            Ok(watch) => watch,
            Err(message) => {
                self.show_local_error(message, cx);
                return;
            }
        };
        let request = state.rpc(t3_core::commands::method::DISPATCH_COMMAND, rollback_command(&command_id, &watch.thread_id, &checkpoint, restore_files));
        self.local_error = None;
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(REWIND_TIMEOUT_SECS)).await;
            let _ = this.update(cx, |view, cx| {
                if view.rewind.active.take().is_some() {
                    view.show_local_error(REWIND_TIMED_OUT.to_owned(), cx);
                    cx.notify();
                }
            });
        });
        self.rewind.active = Some(Active { watch, text, attachments, accepted: false, _timeout: timeout });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                let Some(active) = view.rewind.active.as_mut().filter(|active| active.watch.request_id == command_id) else { return };
                match result {
                    Ok(_) => {
                        active.accepted = true;
                        view.check_rewind(cx);
                    }
                    Err(error) => {
                        view.rewind.active = None;
                        let message = error.to_string();
                        view.show_local_error(if message.trim().is_empty() { REWIND_FAILED.to_owned() } else { message }, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Called on every projection change while a rewind is out.
    pub(super) fn check_rewind(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.rewind.active.as_ref() else { return };
        let state = self.state.read(cx);
        let Some(projection) = self.thread_state(state).projection.as_ref().filter(|projection| projection.thread.id == active.watch.thread_id) else { return };
        match rewind_progress(&active.watch, projection) {
            RewindProgress::Failed(message) => {
                self.rewind.active = None;
                self.show_local_error(message, cx);
            }
            RewindProgress::Done if active.accepted => {
                let Some(active) = self.rewind.active.take() else { return };
                let prompt = recallable_prompt(&active.text);
                let thread_id = active.watch.thread_id;
                self.state.update(cx, |state, cx| state.restore_composer_prompt(&thread_id, prompt, active.attachments, cx));
            }
            _ => return,
        }
        cx.notify();
    }

    /// Leaving the thread drops a pending confirmation; a rewind already sent keeps running on
    /// the server, but nothing here waits for it.
    pub(super) fn reset_rewind(&mut self) {
        self.rewind.counts.clear();
        self.rewind.confirm = None;
        self.rewind.preparing = false;
        self.rewind.prepare_generation = self.rewind.prepare_generation.wrapping_add(1);
        self.rewind.active = None;
    }

    /// `RevertConfirmHost`: an alert dialog with Cancel, "Revert files too" and "Revert and keep
    /// changes". Escape cancels; Enter keeps changes, the dialog's primary action.
    pub(super) fn render_rewind_confirm(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.rewind.confirm.as_ref()?;
        let focus = self.rewind.focus.clone()?;
        let p = palette(cx);
        let viewport = window.viewport_size();
        let button = |id: &'static str, label: &'static str, fill: u32, text: u32, border: u32| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(border))
                .bg(hsla(fill))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(text))
                .cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .child(SharedString::from(label))
        };
        let dialog = div()
            .id("rewind-confirm")
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                "escape" => this.cancel_rewind(window, cx),
                "enter" => this.confirm_rewind(false, window, cx),
                _ => {}
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w_full()
            .max_w(px(512.))
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
                    .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(CONFIRM_TITLE))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(CONFIRM_DESCRIPTION)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(24.))
                    .py(px(16.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .rounded_b(px(15.))
                    .bg(hsla(p.muted).opacity(0.72))
                    .child(button("rewind-cancel", "Cancel", p.popover, p.foreground, p.input).on_click(cx.listener(|this, _, window, cx| this.cancel_rewind(window, cx))))
                    .child(
                        button("rewind-revert-files", REVERT_FILES, p.popover, p.destructive_foreground, p.input)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rewind(true, window, cx))),
                    )
                    .child(
                        button("rewind-keep-changes", KEEP_CHANGES, p.primary, p.primary_foreground, p.primary)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rewind(false, window, cx))),
                    ),
            );
        let backdrop = div()
            .id("rewind-confirm-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.cancel_rewind(window, cx)))
            .child(dialog);
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(backdrop)).with_priority(50).into_any_element())
    }
}

fn parse_attachment_specs(raw: &[Value]) -> Result<Vec<AttachmentSpec>, String> {
    raw.iter()
        .map(|att| {
            let kind = match att.get("type").and_then(Value::as_str) {
                Some("image") => AttachmentKind::Image,
                Some("file") => AttachmentKind::File,
                _ => return Err(UNSUPPORTED_ATTACHMENT.to_owned()),
            };
            let id = att
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| UNSUPPORTED_ATTACHMENT.to_owned())?
                .to_owned();
            let name = att
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("attachment")
                .to_owned();
            let mime_type = att
                .get("mimeType")
                .and_then(Value::as_str)
                .filter(|mime| !mime.is_empty())
                .unwrap_or("application/octet-stream")
                .to_owned();
            Ok(AttachmentSpec { kind, id, name, mime_type })
        })
        .collect()
}

async fn prepare_reverted_attachments(client: &ClientHandle, specs: &[AttachmentSpec]) -> Result<Vec<RestoredAttachment>, String> {
    futures::future::try_join_all(specs.iter().map(|spec| async move {
        let resource = AssetResource::Attachment { attachment_id: spec.id.clone() };
        let minted = client.request(CREATE_ASSET_URL, resource.create_url_request()).await.map_err(|error| {
            let message = error.to_string();
            if message.trim().is_empty() { REWIND_FAILED.to_owned() } else { message }
        })?;
        let asset = decode_asset_url(&minted).map_err(|_| INVALID_ATTACHMENT_URL.to_owned())?;
        if resolve_asset_url(client.environment(), &asset.relative_url).is_none() {
            return Err(INVALID_ATTACHMENT_URL.to_owned());
        }
        let bytes = if asset.relative_url.starts_with("http://") || asset.relative_url.starts_with("https://") {
            t3_client::assets::fetch_signed_media(client, &asset.relative_url).await.map(|media| media.bytes)
        } else {
            t3_client::asset::fetch_asset(client.environment(), &asset.relative_url, MAX_FILE_BYTES as usize)
                .await
                .map_err(|error| error.to_string())
        }
        .map_err(|_| format!("Could not restore attachment: {}", spec.name))?;
        Ok(RestoredAttachment { kind: spec.kind, name: spec.name.clone(), mime_type: spec.mime_type.clone(), bytes })
    }))
    .await
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{Entity, WindowHandle, size};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct RewindHarness {
        view: Entity<ThreadView>,
        composer: Entity<crate::composer::Composer>,
    }

    impl Render for RewindHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn setup_rewind_test(
        cx: &mut gpui::TestAppContext,
        attachments: Vec<Value>,
    ) -> (
        gpui::Entity<AppState>,
        t3_client::supervisor::ClientFixture,
        tokio::runtime::Runtime,
        WindowHandle<RewindHarness>,
    ) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            installed
        });
        state.update(cx, |state, cx| {
            let config = serde_json::from_value(json!({
                "environment": { "environmentId": "fixture-env", "label": "Fixture", "platform": "darwin", "homeDir": "/tmp", "capabilities": {} },
                "providers": [{
                    "instanceId": "replay",
                    "driver": "replay",
                    "displayName": "Replay",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "models": [],
                    "supportsConversationRollback": true
                }],
                "auth": {}
            }))
            .unwrap();
            state.server_config.config = Some(config);
            if let Some(client) = state.client_handle() {
                let shell = std::mem::take(&mut state.shell);
                state.install_active_environment(client, shell, state.server_config.clone(), ConnectionStatus::Connected, cx);
            }
            state.selected_thread = Some("menu-thread".to_owned());
            let item = json!({
                "id": "msg-u1", "threadId": "menu-thread", "runId": "run-1", "nodeId": null,
                "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                "parentItemId": null, "ordinal": 1, "status": "completed",
                "title": null, "startedAt": "2026-01-01T00:00:01Z",
                "completedAt": "2026-01-01T00:00:01Z", "updatedAt": "2026-01-01T00:00:01Z",
                "type": "user_message",
                "messageId": "msg-u1",
                "text": "Earlier prompt",
                "attachments": attachments,
                "inputIntent": "turn_start"
            });
            let projection = serde_json::from_value(json!({
                "thread": {
                    "id": "menu-thread", "projectId": "checkout-b", "title": "menu-thread",
                    "providerInstanceId": "replay",
                    "activeProviderThreadId": "pt-1",
                    "modelSelection": { "instanceId": "replay", "model": "replay" },
                    "runtimeMode": "full-access", "interactionMode": "default",
                    "lineage": { "rootThreadId": "menu-thread" },
                    "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
                },
                "runs": [{
                    "id": "run-1", "ordinal": 1, "providerInstanceId": "replay",
                    "modelSelection": { "instanceId": "replay", "model": "replay" },
                    "userMessageId": "msg-u1", "rootNodeId": null,
                    "status": "completed", "requestedAt": "2026-01-01T00:00:01Z",
                    "startedAt": "2026-01-01T00:00:01Z", "completedAt": "2026-01-01T00:00:02Z"
                }],
                "attempts": [], "nodes": [], "subagents": [],
                "providerSessions": [],
                "providerThreads": [{
                    "id": "pt-1", "providerSessionId": "ps-1", "providerInstanceId": "replay"
                }],
                "providerTurns": [],
                "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
                "checkpointScopes": [],
                "checkpoints": [
                    {
                        "id": "cp-0", "scopeId": "scope-0", "runId": null,
                        "appRunOrdinal": null, "ordinalWithinScope": 0, "status": "ready", "files": [],
                        "capturedAt": "2026-01-01T00:00:00Z"
                    },
                    {
                        "id": "cp-1", "scopeId": "scope-1", "runId": "run-1",
                        "appRunOrdinal": 1, "ordinalWithinScope": 1, "status": "ready", "files": [],
                        "capturedAt": "2026-01-01T00:00:02Z"
                    }
                ],
                "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [{
                    "position": 0,
                    "visibility": "local",
                    "sourceThreadId": "menu-thread",
                    "sourceItemId": "msg-u1",
                    "item": item
                }],
                "updatedAt": "2026-01-01T00:00:02Z"
            }))
            .unwrap();
            state.thread.projection = Some(projection);
            state.thread.synchronized = true;
        });
        let window = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let view = cx.new(|cx| {
                let mut view = ThreadView::new(state.clone(), window, cx);
                view.sync(cx);
                view
            });
            RewindHarness { view, composer }
        });
        (state, fixture, runtime, window)
    }

    #[gpui::test]
    fn rewind_guards_check_permissions_rollback_offline_and_composer_attachments(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let (state, fixture, runtime, window) = setup_rewind_test(
            cx,
            vec![json!({
                "type": "audio", "id": "aud-1", "name": "voice.webm", "mimeType": "audio/webm"
            })],
        );
        let (view, composer) = window.read_with(cx, |h, _| (h.view.clone(), h.composer.clone())).unwrap();
        let _ = window.update(cx, |_, window, cx| {
            // 1. Permission denied silently blocks request_rewind.
            fixture.grants.send_replace(Some(crate::context_menu::project_filter_tests::session(false)));
            view.update(cx, |view, cx| {
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                assert!(view.rewind.confirm.is_none());
                assert!(view.local_error.is_none());
            });

            // 2. Offline environment produces Reconnect <label> error.
            fixture.grants.send_replace(Some(crate::context_menu::project_filter_tests::session(true)));
            state.update(cx, |state, cx| {
                state.set_phase(
                    Phase::Online(ConnectionStatus::Reconnecting { reason: "offline".into(), attempt: 1 }),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                assert!(view.rewind.confirm.is_none());
                assert_eq!(view.local_error.as_deref(), Some("Reconnect Fixture before reverting checkpoints."));
                view.local_error = None;
            });

            // 3. Unsupported rollback returns NO_ROLLBACK.
            state.update(cx, |state, cx| {
                state.set_phase(Phase::Online(ConnectionStatus::Connected), cx);
                state.server_config.config.as_mut().unwrap().providers[0].supports_conversation_rollback = Some(false);
            });
            view.update(cx, |view, cx| {
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                assert_eq!(view.local_error.as_deref(), Some(NO_ROLLBACK));
                view.local_error = None;
            });

            // 4. Re-enable rollback, test confirm_rewind rejects unsupported attachment without RPC.
            state.update(cx, |state, _| {
                state.server_config.config.as_mut().unwrap().providers[0].supports_conversation_rollback = Some(true);
            });
            view.update(cx, |view, cx| {
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                assert!(view.rewind.confirm.is_some());
                view.confirm_rewind(false, window, cx);
                assert_eq!(view.local_error.as_deref(), Some(UNSUPPORTED_ATTACHMENT));
                assert!(!view.rewind.busy());
            });

            // 5. Staged attachment count + message attachments > MAX_ATTACHMENTS blocks confirm_rewind.
            state.update(cx, |state, _| {
                if let Some(projection) = state.thread.projection.as_mut() {
                    let mut value = serde_json::to_value(&*projection.visible_turn_items[0].item).unwrap();
                    value["attachments"] = json!([{
                        "type": "file", "id": "att-1", "name": "notes.txt", "mimeType": "text/plain"
                    }]);
                    projection.visible_turn_items[0].item = std::sync::Arc::new(serde_json::from_value(value).unwrap());
                }
            });
            composer.update(cx, |composer, _| {
                composer.seed_test_attachments("menu-thread", MAX_ATTACHMENTS);
            });
            view.update(cx, |view, cx| {
                view.sync(cx);
                view.local_error = None;
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                view.confirm_rewind(false, window, cx);
                assert_eq!(view.local_error.as_deref(), Some(MAX_ATTACHMENTS_REACHED));
            });

            // 6. Pending attachments in composer block confirm_rewind.
            composer.update(cx, |composer, _| {
                composer.begin_test_preparation("thread:menu-thread");
            });
            view.update(cx, |view, cx| {
                view.local_error = None;
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                view.confirm_rewind(false, window, cx);
                assert_eq!(view.local_error.as_deref(), Some(PENDING_ATTACHMENTS));
            });
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
    async fn rewind_downloads_attachments_before_dispatching_rollback(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let (state, mut fixture, runtime, window) = setup_rewind_test(
            cx,
            vec![json!({
                "type": "file", "id": "att-1", "name": "notes.txt", "mimeType": "text/plain"
            })],
        );
        let (addr, server_task) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = [0u8; 512];
                    let _ = stream.read(&mut buf).await;
                    let body = b"hello-attachment";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(body).await;
                }
            });
            (addr, task)
        });
        let (view, composer) = window.read_with(cx, |h, _| (h.view.clone(), h.composer.clone())).unwrap();
        state.update(cx, |_, cx| cx.emit(crate::app_state::AppEvent::Thread));
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_test_prompt("Current draft", window, cx);
            });
            view.update(cx, |view, cx| {
                view.request_rewind("msg-u1", 0, "Earlier prompt", window, cx);
                view.confirm_rewind(false, window, cx);
                assert!(view.rewind.preparing);
            });
        });

        let (method, _, reply) = fixture.next_request().await;
        assert_eq!(method, CREATE_ASSET_URL);
        let _ = reply.send(Ok(json!({
            "relativeUrl": format!("http://{addr}/notes.txt"),
            "expiresAt": 9999999999i64
        })));
        let (rollback_method, _, rollback_reply) = fixture.next_request().await;
        assert_eq!(rollback_method, t3_core::commands::method::DISPATCH_COMMAND);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let active = view.rewind.active.as_ref().expect("active rewind holds downloaded attachments");
            assert_eq!(active.attachments.len(), 1);
            assert_eq!(active.attachments[0].kind, AttachmentKind::File);
            assert_eq!(active.attachments[0].name, "notes.txt");
            assert_eq!(active.attachments[0].mime_type, "text/plain");
            assert_eq!(active.attachments[0].bytes, b"hello-attachment");
        });
        let _ = rollback_reply.send(Ok(json!({})));
        cx.run_until_parked();

        // Simulate the rolled-back projection arriving and verify Composer receives prompt + attachment.
        state.update(cx, |state, cx| {
            let projection = state.thread.projection.as_mut().unwrap();
            projection.runs[0].status = "rolled_back".to_owned();
            cx.emit(crate::app_state::AppEvent::Thread);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.rewind.active.is_none()));
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.test_prompt(cx), "Current draft\n\nEarlier prompt");
            assert_eq!(
                composer.test_staged_thread_attachments("menu-thread"),
                vec![RestoredAttachment {
                    kind: AttachmentKind::File,
                    name: "notes.txt".to_owned(),
                    mime_type: "text/plain".to_owned(),
                    bytes: b"hello-attachment".to_vec(),
                }]
            );
        });

        server_task.abort();
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
