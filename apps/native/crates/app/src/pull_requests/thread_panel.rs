//! A thread's linked pull requests, as a right panel surface (`ThreadPullRequestsPanel.tsx`):
//! stack-grouped linked pull requests with indentation, watch/copy/open/unlink actions, hover
//! preview cards, an inline thread-link picker (URL / `#42` + quick-pick open PRs from cache),
//! and an embedded in-thread `DetailView` surface when inspecting a linked pull request.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FontWeight, Global, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::store;
use t3_core::commands::method::DISPATCH_COMMAND;
use t3_core::pull_requests::cache::{LIST_CACHE_KEY, PersistedListCache};
use t3_core::pull_requests::detail::{StateKind, checks_state_label};
use t3_core::pull_requests::link::{
    ChangeRequestLink, LinkProject, ThreadLink, can_start_thread_pull_request_watch, is_linked_to_url, link_command, link_validation, parse_change_request_url, resolve_link_input, unlink_command, watch_command,
};
use t3_core::pull_requests::thread_badge::resolve_thread_pull_request_chains;
use t3_core::pull_requests::wire::{Activity, Detail, ListEntry, PrState, Preview, decode};
use t3_core::pull_requests::{PrRef, failure_message, method};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{Button, Size, Variant, checks_glyph, glyph, state_glyph};
use super::detail::{DetailView, has_held_detail, prefetch_into_held};
use super::page::INTENT_PREFETCH_DELAY;
use super::preview::{LinkPreviewController, PreviewTarget};
use super::{LinkPullRequest, OpenPullRequests, read, settle};
use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const ROW_HEIGHT: f32 = 46.;

/// A pull request the page should select when it next shows, and the link that named it, whose
/// fragment says where in it to go.
#[derive(Default)]
pub(super) struct PendingSelection(pub Option<(PrRef, Option<String>)>);

impl Global for PendingSelection {}

struct FlattenedLink {
    link: ThreadLink,
    depth: usize,
    chain_size: usize,
}

pub struct ThreadPullRequestsPanel {
    state: Entity<AppState>,
    selected_thread: Option<String>,
    /// In-thread pull request detail surface (`ThreadPullRequestsPanel` detail view).
    detail: Option<(PrRef, Entity<DetailView>)>,
    /// Inline thread-link picker (`ThreadPullRequestsPanel` inline linker).
    picker_open: bool,
    picker_input: Option<Entity<InputState>>,
    picker_dirty: bool,
    picker_pending: bool,
    picker_error: Option<String>,
    _picker_sub: Option<Subscription>,
    /// Hover preview card for linked pull request rows.
    preview: LinkPreviewController,
    /// Hover-intent prefetch into `DetailView` held cache (`pingdotgg/t3code#32911`).
    intent_target: Option<PrRef>,
    _intent_timer: Option<Task<()>>,
    _intent_fetch: Option<Task<()>>,
    _subscription: Subscription,
}

impl ThreadPullRequestsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let selected_thread = state.read(cx).selected_thread.clone();
        let subscription = cx.subscribe(&state, |this, state, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Selection) {
                let current = state.read(cx).selected_thread.clone();
                if current != this.selected_thread {
                    this.selected_thread = current;
                    this.detail = None;
                    this.picker_open = false;
                    this.picker_dirty = false;
                    this.picker_error = None;
                    this.preview.clear();
                    this.cancel_intent();
                }
                cx.notify();
            }
        });
        ThreadPullRequestsPanel {
            state,
            selected_thread,
            detail: None,
            picker_open: false,
            picker_input: None,
            picker_dirty: false,
            picker_pending: false,
            picker_error: None,
            _picker_sub: None,
            preview: LinkPreviewController::default(),
            intent_target: None,
            _intent_timer: None,
            _intent_fetch: None,
            _subscription: subscription,
        }
    }

    fn cancel_intent(&mut self) {
        self.intent_target = None;
        self._intent_timer = None;
        self._intent_fetch = None;
    }

    fn hover_intent(&mut self, reference: Option<PrRef>, cx: &mut Context<Self>) {
        if self.intent_target == reference {
            return;
        }
        self.cancel_intent();
        let Some(reference) = reference else { return };
        if has_held_detail(self.state.read(cx), &reference, cx) {
            return;
        }
        self.intent_target = Some(reference.clone());
        self._intent_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(INTENT_PREFETCH_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.intent_target.as_ref() != Some(&reference) {
                    return;
                }
                let mut detail_input = reference.payload();
                detail_input["allowStale"] = json!(false);
                let detail_task = read(this.state.read(cx), method::DETAIL, detail_input, decode::<Detail>);
                let activity_task = read(this.state.read(cx), method::ACTIVITY, reference.payload(), decode::<Activity>);
                let reference_done = reference.clone();
                this._intent_fetch = Some(cx.spawn(async move |this, cx| {
                    let (detail_res, activity_res) = futures::future::join(settle(detail_task), settle(activity_task)).await;
                    let _ = this.update(cx, |this, cx| {
                        prefetch_into_held(&this.state, &reference_done, detail_res.ok().as_ref(), activity_res.ok().as_ref(), cx);
                    });
                }));
            });
        }));
    }

    /// `canStartThreadPullRequestWatch` (`pingdotgg/t3code#29867`).
    pub(crate) fn can_start_watch(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)) else {
            return false;
        };
        can_start_thread_pull_request_watch(
            thread.archived_at.is_some(),
            thread.settled_override.as_deref(),
            thread.lineage.relationship_to_parent.as_deref(),
        )
    }

    fn thread_project(&self, cx: &App) -> (Option<String>, Option<LinkProject>) {
        let state = self.state.read(cx);
        let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)) else {
            return (None, None);
        };
        let project_id = thread.project_id.clone();
        let link_project = state
            .shell
            .project(&project_id)
            .and_then(|project| project.repository_identity.as_ref())
            .and_then(LinkProject::from_identity);
        (Some(project_id), link_project)
    }

    fn links(&self, cx: &App) -> (Option<String>, Vec<FlattenedLink>) {
        let state = self.state.read(cx);
        let thread = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id));
        let Some(thread) = thread else { return (None, Vec::new()) };
        let chains = resolve_thread_pull_request_chains(&thread.pull_requests);
        let mut flat = Vec::new();
        for chain in chains {
            let chain_size = chain.layers.len();
            for (index, link) in chain.layers.into_iter().enumerate() {
                flat.push(FlattenedLink {
                    link: link.clone(),
                    depth: chain.depths.get(index).copied().unwrap_or(0),
                    chain_size,
                });
            }
        }
        (Some(thread.project_id.clone()), flat)
    }

    /// Open pull requests from the cold-start cache for the current thread's project that are not
    /// already linked to this thread, filtered by the picker's query text.
    fn quick_pick_entries(&self, query: &str, cx: &App) -> Vec<ListEntry> {
        let cache = PersistedListCache::from_value(store::load_ui_state(LIST_CACHE_KEY));
        self.quick_pick_entries_from_cache(&cache, query, cx)
    }

    fn quick_pick_entries_from_cache(&self, cache: &PersistedListCache, query: &str, cx: &App) -> Vec<ListEntry> {
        let state = self.state.read(cx);
        let Some(thread_id) = state.selected_thread.as_deref() else { return Vec::new() };
        let Some(thread) = state.thread_shell(thread_id) else { return Vec::new() };
        let Some(env_id) = state.environment().or(state.environments.active.as_deref()) else {
            return Vec::new();
        };
        let prefix = format!("{env_id}|");
        let needle = query.trim().to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (key, cached) in &cache.scopes {
            if !key.starts_with(&prefix) {
                continue;
            }
            for entry in &cached.result.entries {
                if entry.project_id != thread.project_id || entry.state != PrState::Open {
                    continue;
                }
                if is_linked_to_url(&thread.pull_requests, &entry.url) {
                    continue;
                }
                let dedupe = format!("{}#{}", entry.repository.to_lowercase(), entry.number);
                if !seen.insert(dedupe) {
                    continue;
                }
                if !needle.is_empty() {
                    let num = format!("#{}", entry.number);
                    let matches = entry.title.to_lowercase().contains(&needle)
                        || entry.repository.to_lowercase().contains(&needle)
                        || num.contains(&needle)
                        || entry.number.to_string().contains(&needle);
                    if !matches {
                        continue;
                    }
                }
                out.push(entry.clone());
                if out.len() >= 6 {
                    return out;
                }
            }
        }
        out
    }

    pub(crate) fn open_detail(&mut self, reference: PrRef, entry: Option<ListEntry>, cx: &mut Context<Self>) {
        self.preview.clear();
        self.cancel_intent();
        let panel = cx.entity().downgrade();
        let on_close = Rc::new({
            let panel = panel.clone();
            move |cx: &mut App| {
                let _ = panel.update(cx, |this, cx| {
                    this.detail = None;
                    cx.notify();
                });
            }
        });
        let on_select_pr = Rc::new({
            let panel = panel.clone();
            move |next: PrRef, cx: &mut App| {
                let _ = panel.update(cx, |this, cx| {
                    this.open_detail(next, None, cx);
                });
            }
        });
        let state = self.state.clone();
        let view = cx.new(|cx| DetailView::new_embedded(state, reference.clone(), entry, on_close, on_select_pr, cx));
        self.detail = Some((reference, view));
        cx.notify();
    }

    pub(crate) fn close_detail(&mut self, cx: &mut Context<Self>) {
        if self.detail.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn toggle_inline_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open {
            self.picker_open = false;
            self.picker_error = None;
            self.picker_dirty = false;
            cx.notify();
            return;
        }
        self.picker_open = true;
        self.picker_dirty = false;
        self.picker_error = None;
        let input = match &self.picker_input {
            Some(existing) => existing.clone(),
            None => {
                let created = cx.new(|cx| InputState::new(window, cx).placeholder("Paste PR URL, #42, or filter open PRs…"));
                self._picker_sub = Some(cx.subscribe_in(&created, window, |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.picker_dirty = true;
                        this.picker_error = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.submit_inline_picker(window, cx),
                    _ => {}
                }));
                self.picker_input = Some(created.clone());
                created
            }
        };
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn submit_inline_picker(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.picker_dirty = true;
        let Some(input) = self.picker_input.as_ref() else { return };
        let query = input.read(cx).value().to_string();
        let (_, project) = self.thread_project(cx);
        let Some(Ok(link)) = resolve_link_input(&query, project.as_ref()) else {
            cx.notify();
            return;
        };
        self.link_change_request(link, cx);
    }

    fn link_quick_pick(&mut self, entry: &ListEntry, cx: &mut Context<Self>) {
        let link = parse_change_request_url(&entry.url).unwrap_or_else(|| ChangeRequestLink {
            host: entry.host.to_lowercase(),
            repository: entry.repository.to_lowercase(),
            number: entry.number,
            url: entry.url.clone(),
        });
        self.link_change_request(link, cx);
    }

    fn link_change_request(&mut self, link: ChangeRequestLink, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).selected_thread.clone() else { return };
        if self.picker_pending {
            return;
        }
        self.picker_pending = true;
        self.picker_error = None;
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, link_command(&uuid_v4(), &thread_id, &link));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.picker_pending = false;
                match result {
                    Ok(_) => {
                        this.picker_open = false;
                        this.picker_dirty = false;
                        this.picker_error = None;
                    }
                    Err(RpcError::Failure(failure)) => {
                        this.picker_error = Some(failure_message(failure.tag.as_deref(), &failure.error, "Could not link the pull request."));
                    }
                    Err(RpcError::Closed) => {
                        this.picker_error = Some("Could not link the pull request.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_watch(&mut self, link: &ThreadLink, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).selected_thread.clone() else { return };
        let enable = link.watch.is_none();
        if enable && !self.can_start_watch(cx) {
            return;
        }
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, watch_command(&uuid_v4(), &thread_id, link, enable));
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                let label = if enable { "Could not watch the pull request" } else { "Could not stop watching" };
                let toast = Toast::new(ToastKind::Error, label).description(error.to_string());
                state.update(cx, |state, cx| state.show_toast(toast, cx));
            }
        })
        .detach();
    }

    fn unlink(&mut self, link: &ThreadLink, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).selected_thread.clone() else { return };
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, unlink_command(&uuid_v4(), &thread_id, link));
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                let toast = Toast::new(ToastKind::Error, "Could not unlink the pull request").description(error.to_string());
                state.update(cx, |state, cx| state.show_toast(toast, cx));
            }
        })
        .detach();
    }

    fn render_inline_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.picker_open {
            return div().into_any_element();
        }
        let Some(input) = self.picker_input.clone() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let (_, project) = self.thread_project(cx);
        let query = input.read(cx).value().to_string();
        let resolved = resolve_link_input(&query, project.as_ref());
        let validation = if self.picker_dirty { link_validation(&query, resolved.as_ref()) } else { None };
        let preview = match &resolved {
            Some(Ok(link)) => Some(format!("{}/{} #{}", link.host, link.repository, link.number)),
            _ => None,
        };
        let can_link = !self.picker_pending && matches!(resolved, Some(Ok(_)));
        let message = validation.or_else(|| self.picker_error.clone());
        let quick_picks = self.quick_pick_entries(&query, cx);

        v_flex()
            .id("thread-pr-inline-picker")
            .flex_none()
            .gap(px(6.))
            .p(px(8.))
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.muted).opacity(0.25))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.picker_open = false;
                this.picker_error = None;
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(div().flex_1().min_w_0().child(Input::new(&input).xsmall()))
                    .child(
                        Button::new("thread-pr-picker-submit", Variant::Default, Size::Xs)
                            .label(if self.picker_pending { "Linking…" } else { "Link" })
                            .disabled(!can_link)
                            .on_click(cx.listener(|this, _, window, cx| this.submit_inline_picker(window, cx))),
                    )
                    .child(
                        Button::new("thread-pr-picker-close", Variant::GhostMuted, Size::IconSm)
                            .icon(Icon::X)
                            .tooltip("Close picker")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.picker_open = false;
                                this.picker_error = None;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(preview, |this, text| {
                this.child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(format!("Will link {text}")))
            })
            .when_some(message, |this, text| {
                this.child(div().text_size(px(11.)).text_color(hsla(p.destructive)).child(text))
            })
            .when(!quick_picks.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap(px(2.))
                        .child(div().text_size(px(10.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Open in this project"))
                        .children(quick_picks.into_iter().enumerate().map(|(index, entry)| {
                            let kind = StateKind::of(entry.state, entry.is_draft);
                            let (icon, color) = state_glyph(kind, cx);
                            let num = format!("#{}", entry.number);
                            let title = entry.title.clone();
                            let picked = entry.clone();
                            h_flex()
                                .id(SharedString::from(format!("thread-pr-quick-{index}")))
                                .gap(px(6.))
                                .px(px(6.))
                                .py(px(4.))
                                .rounded(px(5.))
                                .cursor_pointer()
                                .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
                                .on_click(cx.listener(move |this, _, _, cx| this.link_quick_pick(&picked, cx)))
                                .child(icon.sized(px(13.)).text_color(color).flex_none())
                                .child(div().flex_none().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(num))
                                .child(div().min_w_0().truncate().text_xs().text_color(hsla(p.foreground)).child(title))
                        })),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .child(
                        Button::new("thread-pr-picker-dialog", Variant::GhostMuted, Size::Xs)
                            .label("Open full link dialog…")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(LinkPullRequest), cx)),
                    ),
            )
            .into_any_element()
    }

    fn preview_target(link: &ThreadLink, reference: &PrRef) -> PreviewTarget {
        let fallback = link.snapshot.as_ref().map(|snapshot| Preview {
            project_id: reference.project_id.clone(),
            repository: link.repository.clone(),
            number: link.number,
            title: snapshot.title.clone(),
            url: link.url.clone(),
            author: snapshot.author.clone(),
            state: snapshot.state,
            is_draft: snapshot.is_draft,
            created_at: link.linked_at.clone(),
            updated_at: snapshot.updated_at.clone(),
            observed_at: None,
        });
        PreviewTarget { reference: reference.clone(), url: link.url.clone(), fallback }
    }

    fn row(&self, item: &FlattenedLink, project_id: Option<&str>, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let link = &item.link;
        let snapshot = link.snapshot.as_ref();
        let (icon, color, state_label) = match snapshot {
            Some(snapshot) => {
                let kind = StateKind::of(snapshot.state, snapshot.is_draft);
                let (icon, color) = state_glyph(kind, cx);
                (icon, color, kind.label())
            }
            None => (Icon::GitPullRequestArrow, hsla(p.muted_foreground), "Waiting for host state"),
        };
        let open = snapshot.is_none_or(|snapshot| snapshot.state == PrState::Open);
        let title = snapshot.map_or_else(|| link.repository.clone(), |snapshot| snapshot.title.clone());
        let linked = parse_iso(&link.linked_at).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
        let source = format!("{} · {linked}", link.source_label());
        let reference = project_id.map(|project| PrRef {
            project_id: project.to_owned(),
            host: Some(link.host.clone()),
            repository: link.repository.clone(),
            number: link.number,
        });
        let unlink_label = if link.source == "stack" { "Dismiss from thread" } else { "Unlink from thread" };
        let indent = px((item.depth.min(4) as f32) * 14.);

        let panel_entity = cx.entity();
        let open_here_ref = reference.clone();
        let hover_ref = reference.clone();
        let hover_target = reference.as_ref().map(|reference| Self::preview_target(link, reference));
        let menu_ref = reference.clone();
        let menu_link = link.clone();
        let unlink_target = link.clone();
        let is_watching = link.watch.is_some();
        let can_watch = open && (is_watching || self.can_start_watch(cx));

        h_flex()
            .id(SharedString::from(format!("thread-pr-{index}")))
            .h(px(ROW_HEIGHT))
            .pl(px(8.) + indent)
            .pr(px(6.))
            .gap(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let target = if *hovered { hover_target.clone() } else { None };
                let state = this.state.clone();
                this.preview.hover(&state, target, |panel| &mut panel.preview, cx);
                this.hover_intent(if *hovered { hover_ref.clone() } else { None }, cx);
            }))
            .when(item.depth > 0, |this| {
                this.border_l_2().border_color(hsla(p.border).opacity(0.7))
            })
            .child(glyph(SharedString::from(format!("thread-pr-state-{index}")), icon, color, 16., state_label))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .min_w_0()
                            .child(
                                div()
                                    .id(SharedString::from(format!("thread-pr-number-{index}")))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("#{}", link.number))
                                    .tooltip(move |window, cx| tooltip_view(source.clone().into(), window, cx)),
                            )
                            .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(title))
                            .when(item.chain_size > 1 && item.depth == 0, |this| {
                                this.child(
                                    h_flex()
                                        .flex_none()
                                        .gap(px(2.))
                                        .px(px(4.))
                                        .py(px(1.))
                                        .rounded(px(4.))
                                        .bg(hsla(p.muted))
                                        .text_size(px(10.))
                                        .text_color(hsla(p.muted_foreground))
                                        .child(Icon::Layers.sized(px(10.)))
                                        .child(format!("{}", item.chain_size)),
                                )
                            })
                            .when(open, |this| {
                                this.when(is_watching, |this| {
                                    this.child(glyph(SharedString::from(format!("thread-pr-watch-{index}")), Icon::Eye, hsla(p.muted_foreground), 14., "Watching"))
                                })
                                .children(snapshot.and_then(|s| s.checks_state).map(|state| {
                                    let (icon, color) = checks_glyph(state, cx);
                                    glyph(SharedString::from(format!("thread-pr-checks-{index}")), icon, color, 14., checks_state_label(state))
                                }))
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(hsla(p.muted_foreground))
                            .child(format!("{} · {}", link.repository, link.source_label())),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("thread-pr-menu-{index}")), Variant::GhostMuted, Size::IconSm)
                    .icon(Icon::Ellipsis)
                    .tooltip("Pull request actions")
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        if let Some(reference) = menu_ref.clone() {
                            let panel = panel_entity.clone();
                            let ref_here = reference.clone();
                            menu = menu.item(PopupMenuItem::new("Open details here").on_click(move |_, _, cx| {
                                panel.update(cx, |this, cx| this.open_detail(ref_here.clone(), None, cx));
                            }));
                            let ref_page = reference;
                            menu = menu.item(PopupMenuItem::new("Open in Pull Requests").on_click(move |_, window, cx| {
                                cx.set_global(PendingSelection(Some((ref_page.clone(), None))));
                                window.dispatch_action(Box::new(OpenPullRequests), cx);
                            }));
                        }
                        let copy_url = menu_link.url.clone();
                        let state_copy = panel_entity.clone();
                        menu = menu.item(PopupMenuItem::new("Copy link").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                            let state = state_copy.read(cx).state.clone();
                            state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Info, "Link copied"), cx));
                        }));
                        let open_url = menu_link.url.clone();
                        menu = menu.item(PopupMenuItem::new("Open on host").on_click(move |_, _, cx| {
                            cx.open_url(&open_url);
                        }));
                        if can_watch {
                            let watch_link = menu_link.clone();
                            let panel = panel_entity.clone();
                            let watch_label = if is_watching { "Stop watching" } else { "Watch for changes" };
                            menu = menu.item(PopupMenuItem::new(watch_label).on_click(move |_, _, cx| {
                                panel.update(cx, |this, cx| this.toggle_watch(&watch_link, cx));
                            }));
                        }
                        let unlink_link = menu_link.clone();
                        let panel = panel_entity.clone();
                        menu.separator().item(PopupMenuItem::new(unlink_label).on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| this.unlink(&unlink_link, cx));
                        }))
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("thread-pr-unlink-{index}")), Variant::GhostMuted, Size::IconSm)
                    .icon(Icon::Unlink2)
                    .tooltip(unlink_label)
                    .on_click(cx.listener(move |this, _, _, cx| this.unlink(&unlink_target, cx))),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(reference) = open_here_ref.clone() {
                    this.open_detail(reference, None, cx);
                }
            }))
    }
}

impl Render for ThreadPullRequestsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);

        // In-thread pull request detail surface: embeds DetailView directly in the right panel
        // with a Back button and a shortcut to pop out into the full Pull Requests page.
        if let Some((reference, detail)) = self.detail.clone() {
            let pop_out_ref = reference.clone();
            return v_flex()
                .id("thread-pr-detail-surface")
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .flex_none()
                        .justify_between()
                        .gap(px(6.))
                        .px(px(8.))
                        .py(px(5.))
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .child(
                            Button::new("thread-pr-detail-back", Variant::Ghost, Size::Xs)
                                .icon(Icon::ArrowLeft)
                                .label("Linked pull requests")
                                .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                        )
                        .child(
                            Button::new("thread-pr-detail-popout", Variant::GhostMuted, Size::Xs)
                                .icon(Icon::ExternalLink)
                                .label("Pull Requests")
                                .tooltip("Open in Pull Requests page")
                                .on_click(move |_, window, cx| {
                                    cx.set_global(PendingSelection(Some((pop_out_ref.clone(), None))));
                                    window.dispatch_action(Box::new(OpenPullRequests), cx);
                                }),
                        ),
                )
                .child(div().flex_1().min_h_0().child(detail))
                .into_any_element();
        }

        let (_, links) = self.links(cx);
        let picker = self.render_inline_picker(cx);
        let preview_card = self.preview.render_card(cx);

        if links.is_empty() {
            return v_flex()
                .size_full()
                .min_h_0()
                .child(picker)
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .p(px(24.))
                        .text_center()
                        .child(Icon::Link2.sized(px(24.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("No linked pull requests"))
                        .child(
                            div()
                                .max_w(px(240.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Pull requests the agent opens from this thread land here. Link one yourself from a URL or a number."),
                        )
                        .child(
                            Button::new("thread-pr-link-empty", Variant::Outline, Size::Sm)
                                .icon(Icon::Plus)
                                .label("Link pull request")
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_inline_picker(window, cx))),
                        ),
                )
                .into_any_element();
        }

        let open = links.iter().filter(|item| item.link.snapshot.as_ref().is_none_or(|s| s.state == PrState::Open)).count();
        let count = links.len();
        let entity = cx.entity();
        v_flex()
            .relative()
            .size_full()
            .min_h_0()
            .child(picker)
            .child(
                uniform_list("thread-pull-requests", count, move |range, _, cx| {
                    entity.update(cx, |panel, cx| {
                        let (project_id, links) = panel.links(cx);
                        range
                            .filter_map(|index| links.get(index).map(|item| panel.row(item, project_id.as_deref(), index, cx).into_any_element()))
                            .collect()
                    })
                })
                .flex_1()
                .p(px(6.)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .px(px(8.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .text_size(px(11.))
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("{open} open · {count} linked"))
                    .child(
                        Button::new("thread-pr-link", Variant::Ghost, Size::Xs)
                            .icon(Icon::Plus)
                            .label(if self.picker_open { "Cancel" } else { "Link" })
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_inline_picker(window, cx))),
                    ),
            )
            .when_some(preview_card, |this, card| {
                this.child(div().absolute().bottom(px(36.)).left(px(8.)).right(px(8.)).child(card))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use t3_core::pull_requests::wire::{ListResult, decode};

    use super::*;

    #[gpui::test]
    fn thread_panel_embeds_detail_resets_on_thread_switch_and_filters_quick_picks(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let panel = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);

            let list_result: ListResult = decode(json!({
                "entries": [
                    {
                        "provider": "github", "host": "github.com", "projectId": "p1", "projectTitle": "Repo",
                        "repository": "acme/app", "number": 42, "title": "Already linked PR",
                        "url": "https://github.com/acme/app/pull/42", "author": { "login": "alice" },
                        "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 5, "deletions": 1, "viewerReviewRequested": false, "headBranch": "feat-42", "baseBranch": "main",
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
                    },
                    {
                        "provider": "github", "host": "github.com", "projectId": "p1", "projectTitle": "Repo",
                        "repository": "acme/app", "number": 43, "title": "Unlinked feature PR",
                        "url": "https://github.com/acme/app/pull/43", "author": { "login": "bob" },
                        "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 8, "deletions": 3, "viewerReviewRequested": false, "headBranch": "feat-43", "baseBranch": "main",
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
                    }
                ],
                "providers": [],
                "projectErrors": [],
                "hasMore": false
            }))
            .unwrap();
            let mut cache = PersistedListCache::default();
            cache.put("env-a", "all", list_result, 1_000);
            let roundtrip = PersistedListCache::from_value(Some(serde_json::to_value(&cache).unwrap()));

            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                state.environments.active = Some("env-a".into());
                state.selected_thread = Some("t1".into());
                state.shell.snapshot = Some(serde_json::from_value(json!({
                    "snapshotSequence": 1,
                    "projects": [{
                        "id": "p1",
                        "title": "Repo",
                        "workspaceRoot": "/tmp/repo",
                        "defaultModelSelection": null,
                        "scripts": [],
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }],
                    "threads": [
                        {
                            "id": "t1", "projectId": "p1", "title": "Thread one", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t1" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "pullRequests": [{
                                "host": "github.com",
                                "repository": "acme/app",
                                "number": 42,
                                "url": "https://github.com/acme/app/pull/42",
                                "source": "manual",
                                "linkedAt": "2026-01-02T00:00:00Z"
                            }]
                        },
                        {
                            "id": "t2", "projectId": "p1", "title": "Thread two", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t2" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "pullRequests": []
                        }
                    ],
                    "updatedAt": "2026-01-02T00:00:00Z"
                })).unwrap());
            });

            let panel = cx.new(|cx| ThreadPullRequestsPanel::new(state.clone(), cx));

            // Quick-pick excludes PR #42 (already linked) and surfaces unlinked PR #43.
            let picks = panel.read(cx).quick_pick_entries_from_cache(&roundtrip, "", cx);
            assert_eq!(picks.len(), 1);
            assert_eq!(picks[0].number, 43);
            assert_eq!(panel.read(cx).quick_pick_entries_from_cache(&roundtrip, "43", cx).len(), 1);
            assert!(panel.read(cx).quick_pick_entries_from_cache(&roundtrip, "nonexistent", cx).is_empty());

            // Clicking a linked PR embeds DetailView right inside ThreadPullRequestsPanel.
            let pr_ref = PrRef { project_id: "p1".into(), host: Some("github.com".into()), repository: "acme/app".into(), number: 42 };
            panel.update(cx, |panel, cx| panel.open_detail(pr_ref.clone(), None, cx));
            assert!(panel.read(cx).detail.is_some());

            // Switching to another thread automatically clears the embedded detail surface.
            state.update(cx, |state, cx| {
                state.selected_thread = Some("t2".into());
                cx.emit(AppEvent::Selection);
            });
            panel
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(panel.read(cx).detail.is_none());
        });
    }

    #[gpui::test]
    fn settled_archived_and_subagent_threads_cannot_start_pr_watch(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                state.shell.snapshot = Some(serde_json::from_value(json!({
                    "snapshotSequence": 1,
                    "projects": [{
                        "id": "p1", "title": "Repo", "workspaceRoot": "/tmp/repo",
                        "defaultModelSelection": null, "scripts": [],
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
                    }],
                    "threads": [
                        {
                            "id": "t-active", "projectId": "p1", "title": "Active", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t-active" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "pullRequests": []
                        },
                        {
                            "id": "t-settled", "projectId": "p1", "title": "Settled", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t-settled" }, "status": "idle",
                            "settledOverride": "settled",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "pullRequests": []
                        },
                        {
                            "id": "t-subagent", "projectId": "p1", "title": "Subagent", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t-active", "relationshipToParent": "subagent" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "pullRequests": []
                        }
                    ],
                    "updatedAt": "2026-01-02T00:00:00Z"
                })).unwrap());
                state.selected_thread = Some("t-active".into());
            });
            let panel = cx.new(|cx| ThreadPullRequestsPanel::new(state.clone(), cx));
            assert!(panel.read(cx).can_start_watch(cx));

            state.update(cx, |state, _| state.selected_thread = Some("t-settled".into()));
            assert!(!panel.read(cx).can_start_watch(cx));

            state.update(cx, |state, _| state.selected_thread = Some("t-subagent".into()));
            assert!(!panel.read(cx).can_start_watch(cx));
        });
    }
}
