//! The pull request detail panel (`PullRequestDetailPanel.tsx`): its reads, the header with the
//! primary control and the actions menu (`PullRequestDetailActions.tsx`,
//! `PullRequestActionsMenu.tsx`) with the web's two-step confirmations
//! (`useInlineActionConfirm.ts`), and the Summary, Timeline and Code tabs.

use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::pull_requests::detail::{PrimaryControl, StateKind, change_request_term, merge_method_label, primary_control, resolve_merge_method};
use t3_core::pull_requests::wire::{Action, Activity, Detail, ListEntry, MergeMethod, PrState, decode};
use t3_core::pull_requests::{PrRef, action_done_label, action_input, method};

use super::code::CodeTab;
use super::controls::{BadgeKind, Button, Size, Variant, badge, diff_stat, state_glyph};
use super::page::{PullRequestsPage, action_verb};
use super::{Read, read, settle};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{segment, segmented};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::markdown::MarkdownStore;

type ViewAction = Box<dyn Fn(&mut DetailView, &mut Context<DetailView>)>;

/// How long a first click stays armed (`useInlineActionConfirm`).
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Summary,
    Timeline,
    Code,
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
    merge_method: Option<MergeMethod>,
    armed: Option<Action>,
    pending_action: Option<Action>,
    pub(super) markdown: MarkdownStore,
    pub(super) timeline: Vec<t3_core::pull_requests::detail::TimelineEvent>,
    pub(super) timeline_list: gpui::ListState,
    code: Option<Entity<CodeTab>>,
    _detail: Option<Task<()>>,
    _activity: Option<Task<()>>,
    _disarm: Option<Task<()>>,
}

impl DetailView {
    pub fn new(
        state: Entity<AppState>,
        page: WeakEntity<PullRequestsPage>,
        reference: PrRef,
        entry: Option<ListEntry>,
        merge_method: Option<MergeMethod>,
        cx: &mut Context<Self>,
    ) -> Self {
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
            merge_method,
            armed: None,
            pending_action: None,
            markdown: MarkdownStore::new(cx.entity_id()),
            timeline: Vec::new(),
            timeline_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(600.)),
            code: None,
            _detail: None,
            _activity: None,
            _disarm: None,
        };
        view.refresh(cx);
        view
    }

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
        change_request_term(self.detail.as_ref().map(|d| d.provider.as_str()).or(self.entry.as_ref().map(|e| e.provider.as_str())).unwrap_or("github"))
    }

    fn selected_merge_method(&self) -> Option<MergeMethod> {
        let detail = self.detail.as_ref()?;
        let allowed = detail.allowed_merge_methods();
        (!allowed.is_empty()).then(|| resolve_merge_method(&allowed, self.merge_method, None))
    }

    /// A first click arms; a second within the window runs it (`useInlineActionConfirm`).
    fn confirm(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.armed == Some(action) {
            self.armed = None;
            self._disarm = None;
            self.perform(action, cx);
            return;
        }
        self.armed = Some(action);
        self._disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONFIRM_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                this.armed = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn disarm(&mut self, cx: &mut Context<Self>) {
        if self.armed.take().is_some() {
            self._disarm = None;
            cx.notify();
        }
    }

    fn perform(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.pending_action.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let method = self.selected_merge_method();
        let task = match action {
            Action::Merge => merge_with_detail(state, self.reference.clone(), method),
            _ => read(state, method::RUN_ACTION, action_input(&self.reference, action, method), |_| Ok(())),
        };
        self.pending_action = Some(action);
        let term = self.term();
        let number = self.reference.number;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.pending_action = None;
                let toast = match &result {
                    Ok(()) => Toast::new(ToastKind::Success, action_done_label(action, term)).description(format!("#{number}")),
                    Err(error) => Toast::new(ToastKind::Error, format!("Could not {} #{number}", action_verb(action))).description(error.clone()),
                };
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                if result.is_ok() {
                    let reference = this.reference.clone();
                    if let Some(page) = this.page.upgrade() {
                        page.update(cx, |page, cx| page.acted(&reference, action, cx));
                    }
                    this.refresh(cx);
                }
                cx.notify();
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

    /// The primary control and the actions menu.
    fn render_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let Some(detail) = self.detail.clone() else { return h_flex().into_any_element() };
        let pending = self.pending_action;
        let armed = self.armed;
        let method = self.selected_merge_method();
        let method_label = merge_method_label(method.unwrap_or(MergeMethod::Merge));
        let primary: Option<AnyElement> = match primary_control(&detail) {
            Some(PrimaryControl::Resolve) => Some(badge(format!("Conflicts with {}", detail.base_branch), BadgeKind::Outline, true, cx).text_color(hsla(p.destructive)).into_any_element()),
            Some(PrimaryControl::Ready) => Some(
                Button::new("pr-ready", Variant::Default, Size::Xs)
                    .icon(Icon::GitPullRequestArrow)
                    .label(if pending == Some(Action::Ready) { "Marking ready..." } else { "Ready for review" })
                    .disabled(pending.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.perform(Action::Ready, cx)))
                    .into_any_element(),
            ),
            Some(PrimaryControl::Merge) => Some(
                Button::new("pr-merge", Variant::Default, Size::Xs)
                    .icon(if armed == Some(Action::Merge) { Icon::Check } else { Icon::GitMerge })
                    .label(if pending == Some(Action::Merge) { "Merging...".to_owned() } else if armed == Some(Action::Merge) { "Confirm merge?".to_owned() } else { method_label.to_owned() })
                    .disabled(pending.is_some())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(Action::Merge, cx)))
                    .into_any_element(),
            ),
            Some(PrimaryControl::EnableAutoMerge) => {
                let label = format!("Auto-merge ({})", method_label.to_lowercase());
                Some(
                    Button::new("pr-auto-merge", Variant::Default, Size::Xs)
                        .icon(if armed == Some(Action::EnableAutoMerge) { Icon::Check } else { Icon::GitMerge })
                        .label(if pending == Some(Action::EnableAutoMerge) { "Enabling...".to_owned() } else if armed == Some(Action::EnableAutoMerge) { "Confirm auto-merge?".to_owned() } else { label })
                        .disabled(pending.is_some())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(Action::EnableAutoMerge, cx)))
                        .into_any_element(),
                )
            }
            Some(PrimaryControl::AutoMergeArmed) => Some(badge("Auto-merge", BadgeKind::Info, true, cx).into_any_element()),
            Some(PrimaryControl::Merged | PrimaryControl::Closed) | None => None,
        };
        let close_confirm = (armed == Some(Action::Close)).then(|| {
            Button::new("pr-confirm-close", Variant::DestructiveOutline, Size::Xs)
                .icon(Icon::Check)
                .label(if pending == Some(Action::Close) { "Closing..." } else { "Confirm close?" })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.confirm(Action::Close, cx)))
        });
        let workflows = detail.workflow_approvals_required.filter(|count| *count > 0 && detail.state == PrState::Open && detail.can(Action::ApproveWorkflows)).map(|_| {
            Button::new("pr-approve-workflows", Variant::WarningOutline, Size::Xs)
                .icon(if armed == Some(Action::ApproveWorkflows) { Icon::Check } else { Icon::Hammer })
                .label(if pending == Some(Action::ApproveWorkflows) { "Approving..." } else if armed == Some(Action::ApproveWorkflows) { "Confirm approve?" } else { "Approve workflows to run" })
                .disabled(pending.is_some())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.confirm(Action::ApproveWorkflows, cx)))
        });
        h_flex().flex_none().gap(px(4.)).children(workflows).children(primary).children(close_confirm).child(self.actions_menu(&detail, cx)).into_any_element()
    }

    fn actions_menu(&self, detail: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let detail = detail.clone();
        let pending = self.pending_action.is_some();
        let selected = self.selected_merge_method();
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
            let mut menu = menu.item(PopupMenuItem::new("Refresh from host").on_click(act(&entity, Box::new(|view, cx| view.refresh(cx)))));
            if detail.state == PrState::Open {
                menu = menu.separator();
                let toggle = if detail.is_draft { Action::Ready } else { Action::Draft };
                if detail.can(toggle) {
                    let label = if detail.is_draft { "Ready for review" } else { "Convert to draft" };
                    menu = menu.item(PopupMenuItem::new(label).disabled(pending).on_click(act(&entity, Box::new(move |view, cx| view.perform(toggle, cx)))));
                }
                if !detail.is_draft && detail.can(Action::Merge) && selected.is_some() {
                    menu = menu.item(PopupMenuItem::new("Merge now").disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Action::Merge, cx)))));
                }
                if detail.auto_merge_enabled == Some(true) && detail.can(Action::DisableAutoMerge) {
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
                menu = menu.separator().item(PopupMenuItem::new(format!("Close {term}")).disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Action::Close, cx)))));
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

/// `PullRequests.runAction` for a merge with a merge method: the detail is re-read fresh, the
/// merge refused unless it is still open, ready and allowed, then the method chosen.
pub(super) fn merge_with_detail(state: &AppState, reference: PrRef, preferred: Option<MergeMethod>) -> Read<()> {
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        let mut fresh = reference.payload();
        fresh["allowStale"] = json!(false);
        let detail: Detail = match client.request(method::DETAIL, fresh).await {
            Ok(value) => decode(value)?,
            Err(t3_client::session::RpcError::Failure(f)) => return Err(t3_core::pull_requests::failure_message(f.tag.as_deref(), &f.error, &f.message)),
            Err(_) => return Err(super::NOT_CONNECTED.to_owned()),
        };
        if detail.state != PrState::Open || detail.is_draft || !detail.can(Action::Merge) {
            return Err("This pull request cannot be merged.".to_owned());
        }
        let allowed = detail.allowed_merge_methods();
        if allowed.is_empty() {
            return Err("No merge method is available for this repository.".to_owned());
        }
        let method = resolve_merge_method(&allowed, preferred, None);
        match client.request(method::RUN_ACTION, action_input(&reference, Action::Merge, Some(method))).await {
            Ok(_) => Ok(()),
            Err(t3_client::session::RpcError::Failure(f)) => Err(t3_core::pull_requests::failure_message(f.tag.as_deref(), &f.error, &f.message)),
            Err(_) => Err(super::NOT_CONNECTED.to_owned()),
        }
    }))
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
        v_flex()
            .id("pr-detail")
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.disarm(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.armed.is_some() {
                    this.disarm(cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.render_header(window, cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }
}
