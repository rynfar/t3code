//! The pull request detail panel (`PullRequestDetailPanel.tsx`): its reads, the header with the
//! primary control and the actions menu (`PullRequestDetailActions.tsx`,
//! `PullRequestActionsMenu.tsx`) with the web's two-step confirmations
//! (`useInlineActionConfirm.ts`), the stack merge where the host runs stack actions
//! (`PullRequestStackMenu.tsx`), and the Summary, Timeline and Code tabs.

use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::{MergeGate, PrimaryControl, StateKind, change_request_term, host_label, merge_gate, merge_method_label, primary_control, resolve_merge_method};
use t3_core::pull_requests::wire::{Action, Activity, Detail, ListEntry, MergeMethod, PrState, Stack, decode};
use t3_core::pull_requests::{PrRef, action_failed_label, action_input, action_needs_host_refresh, invalidate_input, method, stack_merge};

use super::code::CodeTab;
use super::controls::{BadgeKind, Button, Size, Variant, badge, diff_stat, state_glyph};
use super::mutations::{self, MergePicks, Mutations};
use super::page::PullRequestsPage;
use super::{read, settle};
use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::right_panel::{segment, segmented};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::markdown::MarkdownStore;

type ViewAction = Box<dyn Fn(&mut DetailView, &mut Context<DetailView>)>;

/// How long a first click stays armed (`useInlineActionConfirm`).
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);
/// The confirmation a menu item arms, shown beside the primary control.
const INLINE_CONFIRM: &str = "pr-confirm-inline";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Summary,
    Timeline,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Action(Action),
    Stack,
}

/// A first click on `element`; only a second click on the same element runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Armed {
    confirm: Confirm,
    element: &'static str,
}

pub struct DetailView {
    pub(super) state: Entity<AppState>,
    page: WeakEntity<PullRequestsPage>,
    pub(super) reference: PrRef,
    entry: Option<ListEntry>,
    pub(super) detail: Option<Detail>,
    pub(super) activity: Option<Activity>,
    pub(super) error: Option<String>,
    pub(super) activity_error: Option<String>,
    pub(super) checks_open: bool,
    tab: Tab,
    /// This pull request's own merge method pick (`mergeMethodSelection`).
    merge_method: Option<MergeMethod>,
    /// `pullRequests.stack` where the host runs stack actions; `None` until it first answers.
    stack: Option<Result<Option<Stack>, String>>,
    stack_pending: bool,
    refreshing: bool,
    armed: Option<Armed>,
    pub(super) markdown: MarkdownStore,
    pub(super) timeline: Vec<t3_core::pull_requests::detail::TimelineEvent>,
    pub(super) timeline_list: gpui::ListState,
    code: Option<Entity<CodeTab>>,
    _detail: Option<Task<()>>,
    _activity: Option<Task<()>>,
    _stack: Option<Task<()>>,
    _disarm: Option<Task<()>>,
    _escape: Option<Subscription>,
    _activation: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl DetailView {
    pub fn new(state: Entity<AppState>, page: WeakEntity<PullRequestsPage>, reference: PrRef, entry: Option<ListEntry>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::Selection | AppEvent::Phase) {
                    this.disarm(cx);
                }
            }),
            cx.observe_global::<Mutations>(|_, cx| cx.notify()),
        ];
        let mut view = DetailView {
            state,
            page,
            reference,
            entry,
            detail: None,
            activity: None,
            error: None,
            activity_error: None,
            checks_open: false,
            tab: Tab::Summary,
            merge_method: None,
            stack: None,
            stack_pending: false,
            refreshing: false,
            armed: None,
            markdown: MarkdownStore::new(cx.entity_id()),
            timeline: Vec::new(),
            timeline_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(600.)),
            code: None,
            _detail: None,
            _activity: None,
            _stack: None,
            _disarm: None,
            _escape: None,
            _activation: None,
            _subscriptions: subscriptions,
        };
        view.refresh(cx);
        view
    }

    /// Re-reads the detail, its activity and stack membership, and the patch if it was read.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let detail = read(state, method::DETAIL, self.reference.payload(), decode::<Detail>);
        let activity = read(state, method::ACTIVITY, self.reference.payload(), decode::<Activity>);
        self._detail = Some(cx.spawn(async move |this, cx| {
            let result = settle(detail).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(detail) => {
                        this.detail = Some(detail);
                        this.error = None;
                        this.refresh_stack(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                this.rebuild_timeline();
                cx.notify();
            });
        }));
        self._activity = Some(cx.spawn(async move |this, cx| {
            let result = settle(activity).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(activity) => {
                        this.activity = Some(activity);
                        this.activity_error = None;
                    }
                    Err(error) => this.activity_error = Some(error),
                }
                this.rebuild_timeline();
                cx.notify();
            });
        }));
        if let Some(code) = &self.code {
            code.update(cx, |code, cx| code.reload(cx));
        }
    }

    /// "Refresh from host": this pull request's host reads are dropped
    /// (`pullRequests.invalidate({ reference })`), then everything re-read.
    pub(super) fn refresh_from_host(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::INVALIDATE, invalidate_input(Some(&self.reference)), |_| Ok(()));
        self.refreshing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let _ = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Stack membership, read only where the host runs stack actions. The last answer stands
    /// while it is re-read; the merge itself checks again before it runs.
    fn refresh_stack(&mut self, cx: &mut Context<Self>) {
        let supported = self.detail.as_ref().is_some_and(|detail| detail.capabilities.stacks == Some(true) && detail.capabilities.stack_actions == Some(true));
        if !supported {
            self._stack = None;
            return;
        }
        let task = read(self.state.read(cx), method::STACK, self.reference.payload(), decode::<Option<Stack>>);
        self._stack = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.stack = Some(result);
                cx.notify();
            });
        }));
    }

    fn retry_stack(&mut self, cx: &mut Context<Self>) {
        self.stack = None;
        self.refresh_stack(cx);
        cx.notify();
    }

    /// `pullRequests.checks` for the Checks section, when the detail's are out of date.
    pub(super) fn refresh_checks(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::CHECKS, self.reference.payload(), t3_core::pull_requests::wire::decode_checks);
        cx.spawn(async move |this, cx| {
            let Ok(Some(checks)) = settle(task).await else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(detail) = this.detail.as_mut() {
                    detail.checks = checks.checks;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn term(&self) -> &'static str {
        change_request_term(self.provider())
    }

    fn provider(&self) -> &str {
        self.detail.as_ref().map(|d| d.provider.as_str()).or(self.entry.as_ref().map(|e| e.provider.as_str())).unwrap_or("github")
    }

    fn gate(&self) -> MergeGate {
        self.detail.as_ref().map_or(MergeGate { single: false, stack_owns: false, blocked: None }, |detail| merge_gate(detail, self.stack.as_ref()))
    }

    fn picks(&self, cx: &gpui::App) -> MergePicks {
        self.page
            .upgrade()
            .map(|page| page.read(cx).merge_picks(&self.reference.project_id, self.merge_method, cx))
            .unwrap_or(MergePicks { current: self.merge_method, ..Default::default() })
    }

    fn selected_merge_method(&self, cx: &gpui::App) -> Option<MergeMethod> {
        let detail = self.detail.as_ref()?;
        let allowed = detail.allowed_merge_methods();
        let picks = self.picks(cx);
        (!allowed.is_empty()).then(|| resolve_merge_method(&allowed, picks.current, picks.project_default, picks.last))
    }

    /// A first click arms; a second on the same control within the window runs it
    /// (`useInlineActionConfirm`). Escape, a press anywhere else, the window losing focus, or a
    /// change of view disarms it.
    fn confirm(&mut self, confirm: Confirm, element: &'static str, cx: &mut Context<Self>) {
        let armed = Armed { confirm, element };
        if self.armed == Some(armed) {
            self.disarm(cx);
            match confirm {
                Confirm::Action(action) => self.perform(action, cx),
                Confirm::Stack => self.merge_stack(cx),
            }
            return;
        }
        self.armed = Some(armed);
        self._disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONFIRM_WINDOW).await;
            let _ = this.update(cx, |this, cx| this.disarm(cx));
        }));
        let this = cx.entity().downgrade();
        self._escape = Some(cx.intercept_keystrokes(move |event, _, cx| {
            if event.keystroke.key == "escape"
                && let Some(this) = this.upgrade()
            {
                this.update(cx, |this, cx| this.disarm(cx));
                cx.stop_propagation();
            }
        }));
        cx.notify();
    }

    fn disarm(&mut self, cx: &mut Context<Self>) {
        self._disarm = None;
        self._escape = None;
        if self.armed.take().is_some() {
            cx.notify();
        }
    }

    fn armed_on(&self, element: &'static str) -> Option<Confirm> {
        self.armed.filter(|armed| armed.element == element).map(|armed| armed.confirm)
    }

    fn perform(&mut self, action: Action, cx: &mut Context<Self>) {
        if !mutations::begin(&self.reference, action, cx) {
            return;
        }
        let task = if action == Action::Merge {
            let picks = self.picks(cx);
            mutations::merge(self.state.read(cx), self.reference.clone(), picks, cx)
        } else {
            let method = self.selected_merge_method(cx);
            mutations::run_action(self.state.read(cx), action_input(&self.reference, action, method), cx)
        };
        let term = self.term();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                mutations::finish(&this.reference, cx);
                if let Err(error) = &result {
                    let toast = Toast::new(ToastKind::Error, action_failed_label(action, term)).description(error.clone());
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    return;
                }
                let reference = this.reference.clone();
                if let Some(page) = this.page.upgrade() {
                    page.update(cx, |page, cx| page.acted(&reference, action, cx));
                }
                if action_needs_host_refresh(action) {
                    this.refresh_from_host(cx);
                } else {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// A stack-scoped merge of this layer and the unmerged ones below it, with the heads the
    /// host must still see (`PullRequestStackMenu`'s `run`).
    fn merge_stack(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(Some(stack))) = &self.stack else { return };
        let Some(method) = self.selected_merge_method(cx) else { return };
        let Some(merge) = stack_merge(stack, &self.reference, method) else { return };
        if !mutations::begin(&self.reference, Action::Merge, cx) {
            return;
        }
        self.stack_pending = true;
        let task = mutations::run_action(self.state.read(cx), merge.input, cx);
        let host = host_label(self.provider());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                mutations::finish(&this.reference, cx);
                this.stack_pending = false;
                let toast = match &result {
                    Ok(()) => Toast::new(ToastKind::Success, "Stack merge request completed").description(format!("{host} merged the stack or added it to its merge queue.")),
                    Err(error) => Toast::new(ToastKind::Error, "Could not merge the stack").description(error.clone()),
                };
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                this.refresh(cx);
                if let Some(page) = this.page.upgrade() {
                    page.update(cx, |page, cx| page.refetch(cx));
                }
            });
        })
        .detach();
    }

    fn select_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        self.merge_method = Some(method);
        if let Some(page) = self.page.upgrade() {
            page.update(cx, |page, _| page.remember_merge_method(method));
        }
        cx.notify();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.disarm(cx);
        self.tab = tab;
        if tab == Tab::Code && self.code.is_none() {
            let state = self.state.clone();
            let reference = self.reference.clone();
            self.code = Some(cx.new(|cx| CodeTab::new(state, reference, cx)));
        }
        cx.notify();
    }

    fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let title = self.detail.as_ref().map(|d| d.title.clone()).or(self.entry.as_ref().map(|e| e.title.clone())).unwrap_or_default();
        let (state, is_draft) = self.detail.as_ref().map(|d| (d.state, d.is_draft)).or(self.entry.as_ref().map(|e| (e.state, e.is_draft))).unwrap_or((PrState::Open, false));
        let kind = StateKind::of(state, is_draft);
        let (icon, color) = state_glyph(kind, cx);
        let url = self.detail.as_ref().map(|d| d.url.clone()).or(self.entry.as_ref().map(|e| e.url.clone()));
        let meta = self.detail.as_ref().map(|d| {
            let author = d.author.as_ref().map_or("ghost".to_owned(), |a| a.login.clone());
            (format!("{author} wants to merge into {} from {}", d.base_branch, d.head_branch), d.additions, d.deletions, d.changed_files)
        });
        v_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(
                h_flex()
                    .h(px(crate::workspace::TITLEBAR_HEIGHT))
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(format!("{} #{}", self.reference.repository, self.reference.number)),
                    )
                    .child(self.render_actions(window, cx))
                    .child(
                        Button::new("pr-detail-close", Variant::Ghost, Size::IconSm)
                            .icon(Icon::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(page) = this.page.upgrade() {
                                    page.update(cx, |page, cx| page.close_detail(cx));
                                }
                            })),
                    ),
            )
            .child(div().text_base().line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title))
            .child(
                h_flex()
                    .gap(px(8.))
                    .min_w_0()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(h_flex().flex_none().gap(px(4.)).text_color(color).child(icon.sized(px(14.))).child(kind.label()))
                    .children(meta.map(|(text, additions, deletions, files)| {
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().child(text))
                            .child(diff_stat(additions, deletions, cx))
                            .child(div().flex_none().child(format!("{files} {}", if files == 1 { "file" } else { "files" })))
                    }))
                    .children(url.map(|url| {
                        Button::new("pr-open-host", Variant::GhostMuted, Size::IconSm).icon(Icon::ExternalLink).tooltip("Open on the host").on_click(move |_, _, cx| cx.open_url(&url))
                    })),
            )
            .child(self.render_tabs(window, cx))
    }

    fn render_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        let option = |id: &'static str, icon: Icon, value: Tab, label: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            segment(id, icon, tab == value, label, window, cx).on_click(cx.listener(move |this, _, _, cx| this.set_tab(value, cx)))
        };
        h_flex().gap(px(8.)).child(
            segmented("pr-detail-tabs", cx)
                .child(option("pr-tab-summary", Icon::MessageSquare, Tab::Summary, "Summary", window, cx).child(tab_label("Summary")))
                .child(option("pr-tab-timeline", Icon::GitCommit, Tab::Timeline, "Timeline", window, cx).child(tab_label("Timeline")))
                .child(option("pr-tab-code", Icon::FileDiff, Tab::Code, "Code", window, cx).child(tab_label("Code"))),
        )
    }

    /// The primary control, the stack merge, the armed menu confirmation and the actions menu.
    /// An armed control that is no longer shown disarms, as the web's does on unmount.
    fn render_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let Some(detail) = self.detail.clone() else { return h_flex().into_any_element() };
        let pending = mutations::pending(&self.reference, cx);
        let busy = pending.is_some();
        let gate = self.gate();
        let method = self.selected_merge_method(cx);
        let method_label = merge_method_label(method.unwrap_or(MergeMethod::Merge));
        let mut shown: Vec<&'static str> = Vec::new();
        let mut confirmable = |id: &'static str, button: Button, view: &Self, cx: &mut Context<Self>| {
            shown.push(id);
            if view.armed_on(id).is_some() { button.on_mouse_down_out(cx.listener(|this, _, _, cx| this.disarm(cx))) } else { button }
        };
        let blocked = gate.blocked.filter(|_| !gate.single && !gate.stack_owns);
        let primary: Option<AnyElement> = match primary_control(&detail, gate.stack_owns) {
            Some(PrimaryControl::Resolve) => Some(badge(format!("Conflicts with {}", detail.base_branch), BadgeKind::Outline, true, cx).text_color(hsla(p.destructive)).into_any_element()),
            Some(PrimaryControl::Ready) => Some(
                Button::new("pr-ready", Variant::Default, Size::Xs)
                    .icon(Icon::GitPullRequestArrow)
                    .label(if pending == Some(Action::Ready) { "Marking ready..." } else { "Ready for review" })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.perform(Action::Ready, cx)))
                    .into_any_element(),
            ),
            Some(PrimaryControl::Merge) => {
                let armed = self.armed_on("pr-merge").is_some();
                let button = Button::new("pr-merge", Variant::Default, Size::Xs)
                    .icon(if armed { Icon::Check } else { Icon::GitMerge })
                    .label(if pending == Some(Action::Merge) { "Merging...".to_owned() } else if armed { "Confirm merge?".to_owned() } else { method_label.to_owned() })
                    .disabled(busy || blocked.is_some())
                    .when_some(blocked, |button, reason| button.tooltip(reason))
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::Merge), "pr-merge", cx)));
                Some(confirmable("pr-merge", button, self, cx).into_any_element())
            }
            Some(PrimaryControl::EnableAutoMerge) => {
                let armed = self.armed_on("pr-auto-merge").is_some();
                let label = format!("Auto-merge ({})", method_label.to_lowercase());
                let button = Button::new("pr-auto-merge", Variant::Default, Size::Xs)
                    .icon(if armed { Icon::Check } else { Icon::GitMerge })
                    .label(if pending == Some(Action::EnableAutoMerge) { "Enabling...".to_owned() } else if armed { "Confirm auto-merge?".to_owned() } else { label })
                    .disabled(busy || blocked.is_some())
                    .when_some(blocked, |button, reason| button.tooltip(reason))
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::EnableAutoMerge), "pr-auto-merge", cx)));
                Some(confirmable("pr-auto-merge", button, self, cx).into_any_element())
            }
            Some(PrimaryControl::AutoMergeArmed) => Some(badge("Auto-merge", BadgeKind::Info, true, cx).into_any_element()),
            Some(PrimaryControl::Merged | PrimaryControl::Closed) | None => None,
        };
        let stack = match &self.stack {
            Some(Ok(Some(stack))) if gate.stack_owns && detail.can(Action::Merge) && method.is_some() => {
                let layer_open = stack.layers.iter().any(|layer| layer.number == self.reference.number && layer.state == PrState::Open);
                layer_open.then(|| {
                    let merge = method.and_then(|method| stack_merge(stack, &self.reference, method));
                    let armed = self.armed_on("pr-merge-stack").is_some();
                    let term = |count: usize| if count == 1 { self.term().to_owned() } else { format!("{}s", self.term()) };
                    let tooltip = merge.as_ref().map_or_else(
                        || "A layer below is closed, a draft, or has no known head.".to_owned(),
                        |merge| format!("Merge stack through #{} into {} ({} {})", self.reference.number, stack.base, merge.count, term(merge.count)),
                    );
                    let label = if self.stack_pending {
                        "Merging stack...".to_owned()
                    } else if armed {
                        merge.as_ref().map_or_else(|| "Merge stack".to_owned(), |merge| format!("Confirm merging {} {}?", merge.count, term(merge.count)))
                    } else {
                        "Merge stack".to_owned()
                    };
                    let button = Button::new("pr-merge-stack", Variant::Default, Size::Xs)
                        .icon(if armed { Icon::Check } else { Icon::GitMerge })
                        .label(label)
                        .tooltip(tooltip)
                        .disabled(busy || merge.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Stack, "pr-merge-stack", cx)));
                    confirmable("pr-merge-stack", button, self, cx)
                })
            }
            _ => None,
        };
        let retry_stack = matches!(self.stack, Some(Err(_))).then(|| {
            Button::new("pr-retry-stack", Variant::Ghost, Size::Xs).label("Retry stack lookup").on_click(cx.listener(|this, _, _, cx| this.retry_stack(cx)))
        });
        let open = detail.state == PrState::Open;
        let can_merge_now = gate.single && open && !detail.is_draft && detail.can(Action::Merge) && method.is_some();
        let inline = self.armed_on(INLINE_CONFIRM).and_then(|confirm| match confirm {
            Confirm::Action(Action::Close) if open && detail.can(Action::Close) => {
                Some(Button::new(INLINE_CONFIRM, Variant::DestructiveOutline, Size::Xs).icon(Icon::Check).label(if pending == Some(Action::Close) { "Closing..." } else { "Confirm close?" }))
            }
            Confirm::Action(Action::Merge) if can_merge_now => Some(Button::new(INLINE_CONFIRM, Variant::Default, Size::Xs).icon(Icon::Check).label("Confirm merge?")),
            _ => None,
        });
        let inline = inline.map(|button| {
            let confirm = self.armed_on(INLINE_CONFIRM);
            let button = button.disabled(busy).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(confirm) = confirm {
                    this.confirm(confirm, INLINE_CONFIRM, cx);
                }
            }));
            confirmable(INLINE_CONFIRM, button, self, cx)
        });
        let workflows = detail.workflow_approvals_required.filter(|count| *count > 0 && open && detail.can(Action::ApproveWorkflows)).map(|_| {
            let armed = self.armed_on("pr-approve-workflows").is_some();
            let button = Button::new("pr-approve-workflows", Variant::WarningOutline, Size::Xs)
                .icon(if armed { Icon::Check } else { Icon::Hammer })
                .label(if pending == Some(Action::ApproveWorkflows) { "Approving..." } else if armed { "Confirm approve?" } else { "Approve workflows to run" })
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::ApproveWorkflows), "pr-approve-workflows", cx)));
            confirmable("pr-approve-workflows", button, self, cx)
        });
        if self.armed.is_some_and(|armed| !shown.contains(&armed.element)) {
            self.armed = None;
            self._disarm = None;
            self._escape = None;
        }
        let menu = self.actions_menu(&detail, gate, cx);
        h_flex().flex_none().gap(px(4.)).children(retry_stack).children(workflows).children(stack).children(primary).children(inline).child(menu).into_any_element()
    }

    fn actions_menu(&self, detail: &Detail, gate: MergeGate, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let detail = detail.clone();
        let pending = mutations::pending(&self.reference, cx).is_some();
        let selected = self.selected_merge_method(cx);
        let refreshing = self.refreshing;
        let term = self.term();
        let act = move |entity: &WeakEntity<DetailView>, run: ViewAction| {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |view, cx| run(view, cx));
                }
            }
        };
        Button::new("pr-actions", Variant::Ghost, Size::IconSm).icon(Icon::Ellipsis).tooltip("More actions").dropdown_menu(move |menu, _, _| {
            let mut menu = menu.item(PopupMenuItem::new(if refreshing { "Refreshing..." } else { "Refresh from host" }).disabled(refreshing).on_click(act(&entity, Box::new(|view, cx| view.refresh_from_host(cx)))));
            if detail.state == PrState::Open {
                menu = menu.separator();
                let toggle = if detail.is_draft { Action::Ready } else { Action::Draft };
                if detail.can(toggle) {
                    let label = if detail.is_draft { "Ready for review" } else { "Convert to draft" };
                    menu = menu.item(PopupMenuItem::new(label).disabled(pending).on_click(act(&entity, Box::new(move |view, cx| view.perform(toggle, cx)))));
                }
                if gate.single && !detail.is_draft && detail.can(Action::Merge) && selected.is_some() {
                    menu = menu.item(PopupMenuItem::new("Merge now").disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Confirm::Action(Action::Merge), INLINE_CONFIRM, cx)))));
                }
                if gate.single && detail.auto_merge_enabled == Some(true) && detail.can(Action::DisableAutoMerge) {
                    menu = menu.item(PopupMenuItem::new("Disable auto-merge").disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.perform(Action::DisableAutoMerge, cx)))));
                }
                let methods = detail.allowed_merge_methods();
                if methods.len() > 1 {
                    menu = menu.separator();
                    for method in methods {
                        menu = menu.item(
                            PopupMenuItem::new(merge_method_label(method))
                                .checked(selected == Some(method))
                                .disabled(pending)
                                .on_click(act(&entity, Box::new(move |view, cx| view.select_merge_method(method, cx)))),
                        );
                    }
                }
            }
            let url = detail.url.clone();
            let link = detail.url.clone();
            let number = format!("#{}", detail.number);
            menu = menu
                .separator()
                .item(PopupMenuItem::new("Open on the host").on_click(move |_, _, cx| cx.open_url(&url)))
                .item(PopupMenuItem::new(format!("Copy {term} link")).on_click(act(&entity, Box::new(move |view, cx| view.copy(link.clone(), "Link", cx)))))
                .item(PopupMenuItem::new(format!("Copy {term} number")).on_click(act(&entity, Box::new(move |view, cx| view.copy(number.clone(), "Number", cx)))));
            if detail.state == PrState::Open && detail.can(Action::Close) {
                menu = menu
                    .separator()
                    .item(PopupMenuItem::new(format!("Close {term}")).disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Confirm::Action(Action::Close), INLINE_CONFIRM, cx)))));
            } else if detail.state == PrState::Closed && detail.can(Action::Reopen) {
                menu = menu.separator().item(PopupMenuItem::new(format!("Reopen {term}")).disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.perform(Action::Reopen, cx)))));
            }
            menu
        })
    }

    fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        if let Some(page) = self.page.upgrade() {
            let what = what.to_owned();
            page.update(cx, |page, cx| page.copy(text, &what, cx));
        }
    }

    fn render_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            Tab::Summary => super::summary::render(self, window, cx),
            Tab::Timeline => super::timeline::render(self, window, cx),
            Tab::Code => self.code.clone().map_or_else(|| div().into_any_element(), |code| div().flex_1().min_h_0().flex().flex_col().child(code).into_any_element()),
        }
    }
}

fn tab_label(label: &'static str) -> impl IntoElement {
    div().ml(px(6.)).text_xs().font_weight(FontWeight::MEDIUM).child(SharedString::from(label))
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        self.markdown.begin_frame();
        let body = match (&self.detail, &self.error) {
            (None, Some(error)) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(24.))
                .text_center()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Could not load this pull request"))
                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(error.clone()))
                .child(Button::new("pr-detail-retry", Variant::Outline, Size::Sm).icon(Icon::RefreshCw).label("Retry").on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                .into_any_element(),
            (None, None) => super::row::ghost("", cx).into_any_element(),
            (Some(_), _) => self.render_tab(window, cx),
        };
        if self._activation.is_none() {
            self._activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.disarm(cx);
                }
            }));
        }
        v_flex()
            .id("pr-detail")
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(self.render_header(window, cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }
}
