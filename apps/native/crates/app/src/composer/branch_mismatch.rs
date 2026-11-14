//! Local checkout branch mismatch banner above the thread composer
//! (`branchMismatchBannerItem` in `ThreadActivityBanners.tsx`, `ChatView.tsx`, `ChatView.logic.ts`,
//! and `handleSwitchCheckoutToThread` in `useThreadLifecycle.tsx`).

use std::collections::HashSet;
use std::time::Duration;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex};
use t3_client::session::RpcError;
use t3_core::auth::SOURCE_CONTROL_WRITE_SCOPE;
use t3_core::commands;
use t3_core::timeline::{BranchMismatch, local_checkout_branch_mismatch, should_show_branch_mismatch_banner};
use t3_core::vcs::{self, RefNameResult, VcsStatus, VcsStatusEvent, apply_status_event, method};
use tokio::sync::mpsc;

use super::{Composer, Target, banner_surface};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::{ghost_button, ghost_icon_button};

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct DismissedBranchMismatches(pub(super) HashSet<String>);
impl gpui::Global for DismissedBranchMismatches {}

#[cfg(all(test, feature = "snapshot"))]
#[derive(Clone, Default)]
pub(crate) struct CheckoutStatusOverride(pub(crate) std::collections::HashMap<String, VcsStatus>);
#[cfg(all(test, feature = "snapshot"))]
impl gpui::Global for CheckoutStatusOverride {}

pub(super) struct CheckoutWatcher {
    connection_id: Option<u64>,
    generation: Option<u64>,
    cwd: String,
    status: Option<VcsStatus>,
    _net: Option<tokio::task::JoinHandle<()>>,
    _ui: Option<Task<()>>,
}

struct CheckoutTarget {
    thread_id: String,
    cwd: String,
    thread_branch: String,
    worktree_path: Option<String>,
}

fn active_checkout_target(composer: &Composer, cx: &App) -> Option<CheckoutTarget> {
    if composer.fixed.is_some() {
        return None;
    }
    let Some(Target::Thread(thread_id)) = &composer.snapshot.target else {
        return None;
    };
    let state = composer.state.read(cx);
    let thread = state.thread_shell(thread_id)?;
    let thread_branch = thread.branch.as_deref().map(str::trim).filter(|b| !b.is_empty())?.to_owned();
    let project = state.shell.snapshot.as_ref()?.projects.iter().find(|project| project.id == thread.project_id)?;
    let cwd = project.workspace_root.trim();
    if cwd.is_empty() {
        return None;
    }
    Some(CheckoutTarget { thread_id: thread_id.clone(), cwd: cwd.to_owned(), thread_branch, worktree_path: thread.worktree_path.clone() })
}

fn spawn_vcs_status_stream(
    client: t3_client::supervisor::ClientHandle,
    runtime: &tokio::runtime::Handle,
    cwd: String,
) -> (tokio::task::JoinHandle<()>, mpsc::UnboundedReceiver<VcsStatusEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let net = runtime.spawn(async move {
        let mut delay = RETRY_MIN;
        let payload = vcs::status_subscription(&cwd);
        loop {
            if let Ok(mut stream) = client.stream(method::SUBSCRIBE_VCS_STATUS, payload.clone()).await {
                while let Some(item) = stream.receiver.recv().await {
                    match item {
                        Ok(value) => {
                            if let Ok(event) = serde_json::from_value::<VcsStatusEvent>(value) {
                                delay = RETRY_MIN;
                                if tx.send(event).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(RpcError::Closed) => break,
                        Err(_) => {}
                    }
                }
            }
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(RETRY_MAX);
        }
    });
    (net, rx)
}

impl Composer {
    pub(super) fn composer_has_content(&self, cx: &App) -> bool {
        !self.draft_prompt(cx).trim().is_empty() || !self.staged().is_empty()
    }

    pub(super) fn sync_checkout_vcs(&mut self, cx: &mut Context<Self>) {
        let Some(target) = active_checkout_target(self, cx).filter(|t| t.worktree_path.is_none()) else {
            self.checkout_vcs = None;
            return;
        };
        #[cfg(all(test, feature = "snapshot"))]
        if let Some(status) = cx.try_global::<CheckoutStatusOverride>().and_then(|map| map.0.get(&target.cwd)).cloned() {
            self.checkout_vcs = Some(CheckoutWatcher {
                connection_id: None,
                generation: None,
                cwd: target.cwd,
                status: Some(status),
                _net: None,
                _ui: None,
            });
            return;
        }
        let state = self.state.read(cx);
        let connection_id = state.connection_id();
        let client = state.client_handle();
        let generation = client.as_ref().map(|handle| *handle.connection_generation().borrow());
        let same = self
            .checkout_vcs
            .as_ref()
            .is_some_and(|watcher| watcher.cwd == target.cwd && watcher.connection_id == connection_id && watcher.generation == generation);
        if same {
            return;
        }
        let Some(client) = client else {
            self.checkout_vcs = None;
            return;
        };
        let (net, mut rx) = spawn_vcs_status_stream(client, state.runtime(), target.cwd.clone());
        let watched_cwd = target.cwd.clone();
        let ui = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let updated = this.update(cx, |composer, cx| {
                    let Some(watcher) = composer.checkout_vcs.as_mut().filter(|watcher| watcher.cwd == watched_cwd) else {
                        return false;
                    };
                    let prev_branch = watcher.status.as_ref().and_then(|s| s.local.ref_name.clone());
                    let prev_dirty = watcher.status.as_ref().is_some_and(|s| s.local.has_working_tree_changes);
                    watcher.status = apply_status_event(watcher.status.take(), event);
                    let next_branch = watcher.status.as_ref().and_then(|s| s.local.ref_name.clone());
                    let next_dirty = watcher.status.as_ref().is_some_and(|s| s.local.has_working_tree_changes);
                    if prev_branch != next_branch || prev_dirty != next_dirty {
                        composer.sync_branch_mismatch(cx);
                        cx.notify();
                    }
                    true
                });
                if !matches!(updated, Ok(true)) {
                    break;
                }
            }
        });
        self.checkout_vcs = Some(CheckoutWatcher {
            connection_id,
            generation,
            cwd: target.cwd,
            status: None,
            _net: Some(net),
            _ui: Some(ui),
        });
    }

    fn active_branch_mismatch(&self, cx: &App) -> Option<(CheckoutTarget, BranchMismatch)> {
        let target = active_checkout_target(self, cx)?;
        let current_branch = self.checkout_vcs.as_ref().filter(|w| w.cwd == target.cwd)?.status.as_ref()?.local.ref_name.as_deref();
        let mismatch = local_checkout_branch_mismatch(&target.thread_id, Some(&target.thread_branch), target.worktree_path.as_deref(), current_branch)?;
        Some((target, mismatch))
    }

    pub(super) fn sync_branch_mismatch(&mut self, cx: &mut Context<Self>) {
        let key = self.active_branch_mismatch(cx).map(|(_, mismatch)| mismatch.key());
        let dismissed = key.as_ref().is_some_and(|key| cx.try_global::<DismissedBranchMismatches>().is_some_and(|set| set.0.contains(key)));
        let has_content = self.composer_has_content(cx);
        let next = match (key, dismissed) {
            (Some(key), false) if has_content || self.revealed_branch_mismatch_key.as_deref() == Some(key.as_str()) => Some(key),
            _ => None,
        };
        if next != self.revealed_branch_mismatch_key {
            self.revealed_branch_mismatch_key = next;
            cx.notify();
        }
    }

    pub(super) fn render_branch_mismatch_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target, mismatch) = self.active_branch_mismatch(cx)?;
        let key = mismatch.key();
        let dismissed = cx.try_global::<DismissedBranchMismatches>().is_some_and(|set| set.0.contains(&key));
        let has_content = self.composer_has_content(cx);
        if !should_show_branch_mismatch_banner(Some(&key), dismissed, has_content, self.revealed_branch_mismatch_key.as_deref()) {
            return None;
        }
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let restoring = self.restoring_branch_threads.contains(&mismatch.thread_id);
        let can_write = self
            .state
            .read(cx)
            .client_handle()
            .is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(SOURCE_CONTROL_WRITE_SCOPE)));
        let dirty = self.checkout_vcs.as_ref().filter(|w| w.cwd == target.cwd).and_then(|w| w.status.as_ref()).is_some_and(|s| s.local.has_working_tree_changes);
        let tooltip_text: SharedString = format!(
            "This thread last ran on {}. Sending will continue on {}.",
            mismatch.thread_branch, mismatch.current_branch
        )
        .into();

        let restore_cwd = target.cwd.clone();
        let restore_thread_id = mismatch.thread_id.clone();
        let restore_branch = mismatch.thread_branch.clone();
        let restore_btn = ghost_button(
            "restore-thread-branch",
            if restoring { "Restoring..." } else { "Restore branch" },
            restoring || !can_write,
            cx.listener(move |this, _, _, cx| {
                this.restore_thread_branch(restore_cwd.clone(), restore_thread_id.clone(), restore_branch.clone(), dirty, cx);
            }),
            cx,
        );

        let dismiss_key = key.clone();
        let dismiss_btn = div()
            .id("dismiss-branch-mismatch-tip")
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Dismiss branch change notice").build(window, cx))
            .child(ghost_icon_button(
                "dismiss-branch-mismatch",
                Icon::X,
                cx.listener(move |this, _, _, cx| {
                    cx.update_default_global::<DismissedBranchMismatches, _>(|set, _| {
                        set.0.insert(dismiss_key.clone());
                    });
                    if this.revealed_branch_mismatch_key.as_deref() == Some(dismiss_key.as_str()) {
                        this.revealed_branch_mismatch_key = None;
                    }
                    cx.notify();
                }),
                cx,
            ));

        let content = h_flex()
            .w_full()
            .min_h(px(24.))
            .gap(px(4.))
            .child(div().flex_none().w(px(24.)).flex().justify_center().child(Icon::GitBranch.render(px(12.), hsla(p.muted_foreground))))
            .child(
                h_flex()
                    .id("branch-mismatch-copy")
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .whitespace_nowrap()
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip_text.clone()).build(window, cx))
                    .child(div().flex_none().font_weight(FontWeight::MEDIUM).line_height(px(24.)).child("Branch changed from"))
                    .child(div().min_w_0().truncate().font_family(mono).text_color(hsla(p.muted_foreground)).child(mismatch.thread_branch)),
            )
            .child(h_flex().flex_none().ml_auto().gap(px(4.)).child(restore_btn).child(dismiss_btn));

        Some(
            banner_surface(false, content, cx)
                .id("branch-mismatch-banner")
                .debug_selector(|| "branch-mismatch-banner".to_owned())
                .into_any_element(),
        )
    }

    fn restore_thread_branch(&mut self, cwd: String, thread_id: String, target_branch: String, dirty: bool, cx: &mut Context<Self>) {
        if self.restoring_branch_threads.contains(&thread_id) {
            return;
        }
        if !dirty {
            self.execute_restore_branch(cwd, thread_id, target_branch, cx);
            return;
        }
        let title = format!("Switch to {target_branch}?");
        let entity = cx.entity().downgrade();
        crate::dialog::ask(
            title,
            "You have uncommitted changes. They'll carry over to the other branch, or block the switch if they conflict.",
            "Switch branch",
            move |_, cx| {
                cx.defer(move |cx| {
                    let _ = entity.update(cx, |this, cx| this.execute_restore_branch(cwd, thread_id, target_branch, cx));
                });
            },
            cx,
        );
    }

    fn execute_restore_branch(&mut self, cwd: String, thread_id: String, target_branch: String, cx: &mut Context<Self>) {
        if !self.restoring_branch_threads.insert(thread_id.clone()) {
            return;
        }
        let state_ref = self.state.read(cx);
        let can_operate = state_ref
            .client_handle()
            .is_some_and(|client| vcs::can_operate_thread(client.session_state().borrow().as_ref()));
        let switch_request = state_ref.rpc(method::VCS_SWITCH_REF, vcs::switch_ref(&cwd, &target_branch));
        let refresh_request = state_ref.rpc(method::VCS_REFRESH_STATUS, vcs::refresh_status(&cwd, true));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let switched = switch_request.await;
            let state = match this.read_with(cx, |composer, _| composer.state.clone()) {
                Ok(state) => state,
                Err(_) => return,
            };
            match switched {
                Ok(value) => {
                    let next_branch = serde_json::from_value::<RefNameResult>(value)
                        .ok()
                        .and_then(|res| res.ref_name)
                        .unwrap_or_else(|| target_branch.clone());
                    let _ = refresh_request.await;
                    if next_branch != target_branch && can_operate {
                        let cmd = vcs::update_thread_branch(&uuid_v4(), &thread_id, Some(&next_branch), None);
                        let update_request = state.read_with(cx, |state, _| state.rpc(commands::method::DISPATCH_COMMAND, cmd));
                        if let Err(error) = update_request.await {
                            let desc = error.to_string();
                            state.update(cx, |state, cx| {
                                state.show_toast(
                                    Toast::new(ToastKind::Error, "Checkout switched, but the thread could not be updated").description(desc),
                                    cx,
                                );
                            });
                        }
                    }
                }
                Err(error) => {
                    let desc = error.to_string();
                    state.update(cx, |state, cx| {
                        state.show_toast(Toast::new(ToastKind::Error, "Failed to switch checkout").description(desc), cx);
                    });
                }
            }
            let _ = this.update(cx, |composer, cx| {
                composer.restoring_branch_threads.remove(&thread_id);
                composer.sync_branch_mismatch(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
