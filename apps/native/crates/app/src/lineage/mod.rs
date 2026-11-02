//! What sits in the composer's place for lineage: `ChildThreadApprovals.tsx` (approvals a
//! subagent or side chat asks the open thread's user for), and for a provider-native subagent,
//! which cannot take messages, `ProviderSubagentBar.tsx` with `NativeChildWorkDialog.tsx`.

mod child_work;
mod relationships;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{ApprovalDecision, respond_to_approval};
use t3_core::lineage::{
    ProviderSubagentStatus, effort_label, format_provider_subagent_status, is_active_status,
    is_provider_native_subagent, provider_subagent_status, reported_model_selection,
};
use t3_core::providers;
use t3_core::timeline::child_inputs;
use t3_core::requests::{PendingApproval, full_access_override_explanation, pending_requests};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::composer::controls::{Button as ActionButton, ButtonSize, ButtonVariant};
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

pub use relationships::{LineageRow, MergeBack, Relationships, StopAction, relationships};

/// One child approval: the child thread, its title, and what it asks.
#[derive(Debug, Clone, PartialEq)]
struct ChildApproval {
    thread_id: String,
    title: String,
    approval: PendingApproval,
}

/// The bar a provider-native subagent shows instead of a composer.
#[derive(Debug, Clone, PartialEq)]
struct SubagentBar {
    thread_id: String,
    parent_id: Option<String>,
    mark: Option<ProviderMark>,
    model_label: String,
    effort_label: Option<String>,
    status: Option<ProviderSubagentStatus>,
    /// Antigravity records the children a subagent started (`getNativeChildWork`).
    child_work: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Snapshot {
    approvals: Vec<ChildApproval>,
    bar: Option<SubagentBar>,
}

pub struct ComposerLineage {
    state: Entity<AppState>,
    snapshot: Snapshot,
    responding: HashSet<String>,
    errors: HashMap<String, String>,
    child_work: Option<Entity<child_work::ChildWorkDialog>>,
    ticker: Option<Task<()>>,
    _subscription: Subscription,
}

impl ComposerLineage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell | AppEvent::SideThread | AppEvent::ServerConfig) {
                this.refresh(cx);
            }
        });
        let mut view = ComposerLineage {
            state,
            snapshot: Snapshot::default(),
            responding: HashSet::new(),
            errors: HashMap::new(),
            child_work: None,
            ticker: None,
            _subscription: subscription,
        };
        view.refresh(cx);
        view
    }

    /// Whether the open thread is a provider-native subagent, which shows the bar, not a composer.
    pub fn replaces_composer(&self) -> bool {
        self.snapshot.bar.is_some()
    }

    fn read_snapshot(&self, cx: &Context<Self>) -> Snapshot {
        let state = self.state.read(cx);
        let Some(thread_id) = state.selected_thread.as_deref() else { return Snapshot::default() };
        let mut approvals = Vec::new();
        if let Some(snapshot) = &state.shell.snapshot {
            let children = child_inputs(&snapshot.threads, thread_id, false).map(|inputs| inputs.children).unwrap_or_default();
            for child in children.into_iter().filter(|child| child.approval) {
                let Some(projection) = state.thread_state(&child.thread_id).projection.as_ref() else { continue };
                for approval in pending_requests(projection).approvals {
                    approvals.push(ChildApproval { thread_id: child.thread_id.clone(), title: child.title.clone(), approval });
                }
            }
        }
        Snapshot { approvals, bar: Self::read_bar(thread_id, state) }
    }

    fn read_bar(thread_id: &str, state: &AppState) -> Option<SubagentBar> {
        let shell = state.thread_shell(thread_id)?;
        if !is_provider_native_subagent(&shell.lineage, shell.creation_source.as_deref()) {
            return None;
        }
        let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let selection = &shell.model_selection;
        let config = state.server_config.config.as_ref();
        let provider = config.and_then(|config| config.provider(&selection.instance_id));
        let model = provider.and_then(|provider| provider.models.iter().find(|model| model.slug == selection.model));
        let entries = config.map(|config| providers::entries(config, Some(&shell.project_id))).unwrap_or_default();
        let mark = entries.iter().find(|entry| entry.instance_id() == selection.instance_id).map(|entry| ProviderMark::of(entry, &entries));
        let reported = projection.and_then(reported_model_selection);
        Some(SubagentBar {
            thread_id: thread_id.to_owned(),
            parent_id: shell.lineage.parent_thread_id.clone(),
            mark,
            model_label: model.map_or_else(|| selection.model.clone(), |model| providers::trigger_model_name(model).to_owned()),
            effort_label: effort_label(selection, model, reported.as_ref()),
            status: projection.and_then(provider_subagent_status),
            child_work: provider.is_some_and(|provider| provider.driver == "antigravity"),
        })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.read_snapshot(cx);
        if next == self.snapshot {
            return;
        }
        let keep: HashSet<&str> = next.approvals.iter().map(|entry| entry.approval.request_id.as_str()).collect();
        self.responding.retain(|id| keep.contains(id.as_str()));
        self.errors.retain(|id, _| keep.contains(id.as_str()));
        if next.bar.as_ref().map(|bar| &bar.thread_id) != self.snapshot.bar.as_ref().map(|bar| &bar.thread_id) {
            self.child_work = None;
        }
        self.snapshot = next;
        self.sync_ticker(cx);
        cx.notify();
    }

    /// The elapsed status ticks once a second while the subagent works, and not otherwise.
    fn sync_ticker(&mut self, cx: &mut Context<Self>) {
        let live = self.snapshot.bar.as_ref().and_then(|bar| bar.status.as_ref()).is_some_and(|status| is_active_status(&status.status));
        if !live {
            self.ticker = None;
            return;
        }
        if self.ticker.is_none() {
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    fn respond(&mut self, thread_id: String, request_id: String, decision: ApprovalDecision, cx: &mut Context<Self>) {
        if !self.responding.insert(request_id.clone()) {
            return;
        }
        self.errors.remove(&request_id);
        cx.notify();
        let command = respond_to_approval(&uuid_v4(), &thread_id, &request_id, decision, None);
        let view = cx.entity().downgrade();
        self.state.update(cx, |state, cx| {
            state.dispatch_then(command, None, cx, move |_, ok, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.responding.remove(&request_id);
                    if !ok {
                        this.errors.insert(request_id, "Failed to submit approval decision.".to_owned());
                    }
                    cx.notify();
                });
            });
        });
    }

    fn render_approval(&self, entry: &ChildApproval, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let approval = &entry.approval;
        let responding = self.responding.contains(&approval.request_id);
        let (primary, more) = approval.split_options();
        let buttons: Vec<AnyElement> = primary
            .into_iter()
            .chain(more)
            .map(|option| {
                let (thread_id, request_id, decision) = (entry.thread_id.clone(), approval.request_id.clone(), option.decision);
                let variant = if decision == ApprovalDecision::Accept { ButtonVariant::Default } else { ButtonVariant::Outline };
                ActionButton::new(SharedString::from(format!("child-approval-{}-{}", approval.request_id, decision.as_str())), variant, ButtonSize::Xs)
                    .label(option.label.clone())
                    .disabled(responding || !approval.live)
                    .on_click(cx.listener(move |this, _, _, cx| this.respond(thread_id.clone(), request_id.clone(), decision, cx)))
                    .into_any_element()
            })
            .collect();
        let muted = hsla(p.muted_foreground);
        let note = |text: String| div().mt(px(8.)).text_color(muted).child(text);
        let detail = approval.detail.clone().filter(|detail| !detail.is_empty()).unwrap_or_else(|| approval.request_kind.clone());
        v_flex()
            .w_full()
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .text_xs()
            .text_color(hsla(p.foreground))
            .child(div().mb(px(8.)).font_weight(FontWeight::MEDIUM).child(format!("Approval needed from {}", entry.title)))
            .child(
                div()
                    .id(SharedString::from(format!("child-approval-detail-{}", approval.request_id)))
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(detail),
            )
            .children(approval.app_name.clone().map(note))
            .when(!approval.live, |this| {
                this.child(div().mt(px(8.)).text_color(hsla(p.warning)).child("Provider process is gone. Interrupt or restart the child thread to respond."))
            })
            .children(approval.reason.clone().map(note))
            .children(approval.full_access_override.as_deref().map(|value| note(full_access_override_explanation(value).to_owned())))
            .child(h_flex().mt(px(8.)).flex_wrap().gap(px(8.)).children(buttons))
            .children(self.errors.get(&approval.request_id).map(|error| div().mt(px(8.)).text_color(hsla(p.destructive)).child(error.clone())))
            .into_any_element()
    }

    fn render_bar(&self, bar: &SubagentBar, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let muted = hsla(p.muted_foreground);
        let status = format_provider_subagent_status(bar.status.as_ref(), chrono::Utc::now().timestamp_millis());
        // The web sizes this by the composer: a narrow column puts the status on its own line.
        let narrow = f32::from(window.viewport_size().width) < 900.;
        let parent = bar.parent_id.clone();
        let surface = hsla(if dark { p.surface_raised } else { p.card });
        let outline = if dark { gpui::white().opacity(0.05) } else { gpui::black().opacity(0.08) };
        let model = h_flex()
            .min_w_0()
            .max_w_full()
            .gap(px(8.))
            .children(bar.mark.clone().map(|mark| ProviderIcon::new(mark).size(px(16.))))
            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(bar.model_label.clone()))
            .children(bar.effort_label.clone().map(|effort| div().flex_none().text_color(muted).child(effort)));
        h_flex()
            .w_full()
            .min_h(px(48.))
            .items_center()
            .gap(px(12.))
            .py(px(8.))
            .pl(px(20.))
            .pr(px(8.))
            .rounded(px(16.))
            .border_1()
            .border_color(outline)
            .bg(surface)
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .when(narrow, |this| this.flex_col().items_start().gap(px(2.)))
                    .when(!narrow, |this| this.items_center().gap(px(12.)))
                    .child(model)
                    .child(div().min_w_0().max_w_full().truncate().text_color(muted).when(narrow, |this| this.text_xs()).child(status)),
            )
            .when(!narrow, |this| this.child(div().flex_none().text_color(muted).child("Runs on its own")))
            .when(bar.child_work, |this| {
                this.child(
                    Button::new("subagent-work")
                        .variant(Variant::Ghost)
                        .size(Size::Xs)
                        .icon(Icon::ListTree)
                        .label("Subagent work")
                        .on_click(cx.listener(|this, _, window, cx| this.open_child_work(window, cx))),
                )
            })
            .children(parent.map(|parent| {
                Button::new("subagent-open-parent")
                    .variant(Variant::Ghost)
                    .size(Size::Xs)
                    .icon(Icon::ArrowUpLeft)
                    .label("Open parent")
                    .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |state, cx| state.select_thread(&parent, cx))))
            }))
            .into_any_element()
    }

    fn open_child_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.snapshot.bar.as_ref().map(|bar| bar.thread_id.clone()) else { return };
        let dialog = cx.new(|cx| child_work::ChildWorkDialog::new(self.state.clone(), thread_id, window, cx));
        cx.subscribe(&dialog, |this, _, _: &child_work::Dismissed, cx| {
            this.child_work = None;
            cx.notify();
        })
        .detach();
        self.child_work = Some(dialog);
        cx.notify();
    }
}

impl Render for ComposerLineage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.snapshot.approvals.is_empty() && self.snapshot.bar.is_none() {
            return div().into_any_element();
        }
        let entries = self.snapshot.approvals.clone();
        let approvals: Vec<AnyElement> = entries.iter().map(|entry| self.render_approval(entry, cx)).collect();
        let bar = self.snapshot.bar.clone().map(|bar| self.render_bar(&bar, window, cx));
        let has_approvals = !approvals.is_empty();
        let has_bar = bar.is_some();
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(crate::theme::rem(48.))
            .when(has_bar, |this| this.pb(px(16.)).pt(px(8.)))
            .child(
                v_flex()
                    .w_full()
                    .when_some(crate::theme::chat_max_width(cx), |this, width| this.max_w(width))
                    .when(has_approvals, |this| {
                        this.child(v_flex().id("child-thread-approvals").max_h(px(320.)).overflow_y_scroll().gap(px(8.)).mb(px(8.)).children(approvals))
                    })
                    .children(bar),
            )
            .children(self.child_work.clone())
            .into_any_element()
    }
}
