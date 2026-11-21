//! Thread hover card (`SidebarThreadTooltip` in `sidebar/SidebarThreadRow.tsx`, `ThreadHoverCard.tsx`).
//! Built when the glass tooltip opens, from what the app already holds, and dropped with it; the
//! one read it makes, the checkout's branch, goes with it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{AnyElement, App, AppContext as _, Bounds, BoxShadow, Context, Entity, EntityId, FontWeight, IntoElement, ListState, ParentElement, Pixels, Render, SharedString, Styled, Task, WeakEntity, Window, div, point, prelude::*, px, rgba};
use t3_core::pull_requests::thread_badge::{BadgeState, PrGlyph, PrTone};
use t3_core::sidebar::hover as model;
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::sidebar::thread::SidebarThread;

use crate::app_state::AppState;
use crate::icons::Icon;
use crate::project_icon::ProjectVisual;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{Palette, hsla, palette};

/// `max-w-80`.
const MAX_WIDTH: f32 = 320.;
/// `Math.min(depth, 3) * 0.75rem`.
const MAX_PR_DEPTH: usize = 3;

pub(super) fn pr_icon(glyph: PrGlyph) -> Icon {
    match glyph {
        PrGlyph::PullRequest => Icon::GitPullRequestArrow,
        PrGlyph::Draft => Icon::GitPullRequestDraft,
        PrGlyph::Closed => Icon::GitPullRequestClosed,
        PrGlyph::Merged => Icon::GitMerge,
        PrGlyph::Stack => Icon::Layers,
    }
}

pub(super) fn pr_tone(tone: PrTone, p: Palette) -> u32 {
    match tone {
        PrTone::Open => p.pill_completed,
        PrTone::Draft | PrTone::Muted => p.muted_foreground,
        PrTone::Closed => p.pill_failed,
        PrTone::Merged => p.pill_plan,
    }
}

/// The row a card describes, captured at render time; everything else is read when it opens.
#[derive(Clone)]
pub(super) struct HoverTarget {
    pub(super) state: Entity<AppState>,
    pub(super) key: Option<ThreadKey>,
    pub(super) thread_id: String,
    pub(super) project: Option<String>,
    pub(super) favicon: ProjectVisual,
    pub(super) is_repo: Option<bool>,
}

struct ModelLine {
    mark: ProviderMark,
    accent: Option<u32>,
    text: SharedString,
}

struct PrLine {
    icon: Icon,
    tone: Option<PrTone>,
    number: u64,
    title: SharedString,
    depth: usize,
    stack: Option<SharedString>,
}

/// Lets a running read go when the card or watcher closes.
pub(super) struct AbortOnDrop<T>(pub(super) tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct ThreadHoverCard {
    title: SharedString,
    project: Option<SharedString>,
    favicon: ProjectVisual,
    environment: Option<(SharedString, Icon)>,
    branch: Option<SharedString>,
    branch_mismatch: bool,
    model: Option<ModelLine>,
    handed_off: Option<SharedString>,
    terminals: usize,
    /// The error line and whether it is a usage limit.
    error: Option<bool>,
    pull_requests: Vec<PrLine>,
    _checkout: Option<Task<()>>,
}

fn find_thread<'a>(state: &'a AppState, target: &HoverTarget) -> Option<&'a SidebarThread> {
    match &target.key {
        Some(key) => state.sidebar.fleet.thread(key).map(|row| &row.thread),
        None => state.sidebar.model.thread(&target.thread_id),
    }
}

/// The checkout comparison: answered already (snapshot fixtures), or one read in flight.
enum Checkout {
    #[cfg(feature = "snapshot")]
    Known(bool),
    Read(AbortOnDrop<Option<t3_core::vcs::VcsLocalStatus>>),
}

impl ThreadHoverCard {
    pub(super) fn new(target: &HoverTarget, cx: &mut Context<Self>) -> Self {
        let entity = target.state.clone();
        let state = entity.read(cx);
        let Some(raw_thread) = find_thread(state, target) else {
            return Self::empty(target);
        };
        let stripped = raw_thread.clone().without_removed_checkout(target.is_repo);
        let thread = &stripped;
        let config = match &target.key {
            Some(key) => state.fleet.environment(&key.environment_id).and_then(|entry| entry.config.config.as_ref()),
            None => state.server_config.config.as_ref(),
        };
        let entries = config.map(|config| t3_core::providers::entries(config, Some(&thread.project_id))).unwrap_or_default();
        let model = entries.iter().find(|entry| entry.instance_id() == model::model_instance_id(thread)).map(|entry| ModelLine {
            mark: ProviderMark::of(entry, &entries),
            accent: t3_core::providers::accent_color(entry.provider).filter(|_| t3_core::providers::shows_instance_badge(entry, &entries)),
            text: model::model_text(thread, entry, &entries).into(),
        });
        // Terminal metadata follows the current connection only.
        let current_environment = target.key.as_ref().is_none_or(|key| state.environments.active.as_deref() == Some(key.environment_id.as_str()));
        let terminals = if current_environment {
            cx.try_global::<crate::terminal::RunningTerminals>().and_then(|running| running.by_thread.get(&thread.id)).map_or(0, Vec::len)
        } else {
            0
        };
        // `useSupportsMultiplePullRequests`: the owning environment's config is loaded.
        let pull_requests = if config.is_some() {
            model::pull_request_list_lines(&thread.pull_requests)
                .into_iter()
                .map(|line| {
                    let state = line.link.snapshot.as_ref().map(|snapshot| BadgeState::of(snapshot.state, snapshot.is_draft));
                    PrLine {
                        icon: pr_icon(state.map_or(PrGlyph::PullRequest, BadgeState::glyph)),
                        tone: state.map(BadgeState::tone),
                        number: line.link.number,
                        title: line.link.snapshot.as_ref().map_or_else(|| line.link.repository.clone(), |snapshot| snapshot.title.clone()).into(),
                        depth: line.depth.min(MAX_PR_DEPTH),
                        stack: line.stack_label().map(Into::into),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let checkout = Self::checkout(target, state, thread, cx);
        let mut card = ThreadHoverCard {
            title: thread.title.clone().into(),
            project: target.project.clone().map(Into::into),
            favicon: target.favicon.clone(),
            environment: config.and_then(|config| {
                let label = config.environment.label.trim();
                (!label.is_empty()).then(|| (SharedString::from(label.to_owned()), Icon::machine(config)))
            }),
            branch: thread.branch.clone().filter(|branch| !branch.is_empty()).map(Into::into),
            branch_mismatch: false,
            model,
            handed_off: model::handed_off_from(thread, &entries).map(Into::into),
            terminals,
            error: thread.runtime.as_ref().filter(|runtime| runtime.last_error.is_some()).map(|runtime| runtime.last_error_class.as_deref() == Some("usage_limit")),
            pull_requests,
            _checkout: None,
        };
        match checkout {
            #[cfg(feature = "snapshot")]
            Some(Checkout::Known(mismatch)) => card.branch_mismatch = mismatch,
            Some(Checkout::Read(mut read)) => {
                card._checkout = Some(cx.spawn(async move |this, cx| {
                    let status = (&mut read.0).await.ok().flatten();
                    let _ = this.update(cx, |card, cx| {
                        if status.as_ref().is_some_and(|status| !status.is_repo) {
                            card.branch = None;
                            card.branch_mismatch = false;
                        } else {
                            let current = status.and_then(|status| status.ref_name);
                            card.branch_mismatch = model::branch_mismatch(&stripped, current.as_deref());
                        }
                        cx.notify();
                    });
                }));
            }
            None => {}
        }
        card
    }

    fn empty(target: &HoverTarget) -> Self {
        ThreadHoverCard {
            title: SharedString::default(),
            project: target.project.clone().map(Into::into),
            favicon: target.favicon.clone(),
            environment: None,
            branch: None,
            branch_mismatch: false,
            model: None,
            handed_off: None,
            terminals: 0,
            error: None,
            pull_requests: Vec::new(),
            _checkout: None,
        }
    }

    /// Reads the thread's checkout (worktree or project root) once, for a thread that records
    /// a branch or worktree, from the environment that owns the thread.
    fn checkout(target: &HoverTarget, state: &AppState, thread: &SidebarThread, cx: &App) -> Option<Checkout> {
        if target.is_repo == Some(false) {
            return None;
        }
        let root = match &target.key {
            Some(key) => state
                .scoped_project(&t3_core::sidebar::grouping::ProjectRef { environment_id: key.environment_id.clone(), project_id: thread.project_id.clone() })
                .map(|project| project.project.workspace_root.as_str()),
            None => state.shell.project(&thread.project_id).map(|project| project.workspace_root.as_str()),
        };
        #[cfg(feature = "snapshot")]
        if state.sidebar.snapshot_fixture {
            let cwd = model::checkout_to_compare(thread, root)?.to_owned();
            let current = cx.try_global::<crate::snapshot::CheckoutFixture>().and_then(|fixture| fixture.0.get(&cwd).cloned());
            return Some(Checkout::Known(model::branch_mismatch(thread, current.as_deref())));
        }
        #[cfg(not(feature = "snapshot"))]
        let _ = cx;
        if thread.branch.as_deref().is_none_or(str::is_empty) && thread.worktree_path.as_deref().is_none_or(str::is_empty) {
            return None;
        }
        let cwd = thread
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .or_else(|| root.map(str::trim).filter(|path| !path.is_empty()))?
            .to_owned();
        let client: std::pin::Pin<Box<dyn std::future::Future<Output = Option<t3_client::supervisor::ClientHandle>> + Send>> = match &target.key {
            None => {
                let client = state.client_handle();
                Box::pin(async move { client })
            }
            Some(key) => {
                let owner = state.scoped_environment(&key.environment_id)?.summary.owner.clone();
                let destination = state.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
                Box::pin(async move { destination.await.ok().map(|destination| destination.client) })
            }
        };
        let read = AbortOnDrop(state.runtime().spawn(async move {
            let value = client.await?.request(t3_core::vcs::method::VCS_REFRESH_STATUS, t3_core::vcs::refresh_status(&cwd, true)).await.ok()?;
            serde_json::from_value::<t3_core::vcs::VcsLocalStatus>(value).ok()
        }));
        Some(Checkout::Read(read))
    }
}

fn line(icon: AnyElement, text: impl Into<SharedString>, p: Palette) -> gpui::Div {
    div().flex().items_center().gap(px(8.)).min_w_0().child(icon).child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(text.into()))
}

/// `glass` tooltip shadow: the toast's (design system §4.3).
fn toast_shadow() -> Vec<BoxShadow> {
    vec![
        BoxShadow { color: gpui::black().opacity(0.25), offset: point(px(0.), px(20.)), blur_radius: px(25.), spread_radius: px(-5.), inset: false },
        BoxShadow { color: gpui::black().opacity(0.25), offset: point(px(0.), px(8.)), blur_radius: px(10.), spread_radius: px(-6.), inset: false },
    ]
}

impl Render for ThreadHoverCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let project = self.project.clone().map(|project| {
            let icon = self.favicon.render(crate::project_icon::size(12., cx), hsla(p.icon_muted), cx);
            line(div().flex_none().child(icon).into_any_element(), project, p).debug_selector(|| "thread-hover-project".into())
        });
        let environment = self.environment.clone().map(|(label, icon)| line(icon.render(px(12.), muted).into_any_element(), label, p).debug_selector(|| "thread-hover-environment".into()));
        let branch = self.branch.clone().map(|branch| line(Icon::GitBranch.render(px(12.), muted).into_any_element(), branch, p).debug_selector(|| "thread-hover-branch".into()));
        let mismatch = self.branch_mismatch.then(|| {
            let warning = hsla(p.warning);
            div()
                .debug_selector(|| "thread-hover-branch-mismatch".into())
                .flex()
                .items_start()
                .gap(px(8.))
                .min_w_0()
                .text_color(warning)
                .child(div().mt(px(2.)).flex_none().child(Icon::CircleAlert.render(px(12.), warning)))
                .child(div().flex_1().min_w_0().child(crate::i18n::t(cx, "You're currently checked out on another branch.")))
        });
        let model = self.model.as_ref().map(|model| {
            let icon = div()
                .relative()
                .flex_none()
                .child(ProviderIcon::new(model.mark.clone()).size(px(12.)).faded(0.6, true))
                .children(model.accent.map(|accent| div().absolute().right(px(-2.)).bottom(px(-2.)).size(px(8.)).rounded_full().border_1().border_color(hsla(p.dropdown_surface)).bg(rgba(accent))));
            line(icon.into_any_element(), model.text.clone(), p).debug_selector(|| "thread-hover-model".into())
        });
        let handed_off = self.handed_off.clone().map(|text| line(Icon::ArrowRightLeft.render(px(12.), muted).into_any_element(), text, p).debug_selector(|| "thread-hover-handoff".into()));
        let terminals = (self.terminals > 0).then(|| {
            line(Icon::Terminal.render(px(12.), hsla(p.terminal_running)).into_any_element(), model::terminal_process_label(self.terminals), p).debug_selector(|| "thread-hover-terminals".into())
        });
        let error = self.error.map(|usage_limit| {
            let color = if usage_limit { hsla(p.warning) } else { hsla(p.error) };
            div()
                .debug_selector(move || if usage_limit { "thread-hover-usage-limit".into() } else { "thread-hover-error".into() })
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .text_color(color)
                .child(Icon::CircleAlert.render(px(12.), color))
                .child(div().min_w_0().truncate().child(crate::i18n::t(cx, if usage_limit { "Usage limit reached" } else { "Error occurred" })))
        });
        let mono = crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx);
        let pull_requests = (!self.pull_requests.is_empty()).then(|| {
            div()
                .debug_selector(|| "thread-hover-pull-requests".into())
                .border_t_1()
                .border_color(hsla(p.border).opacity(0.6))
                .pt(px(8.))
                .pl(px(2.))
                .text_color(muted)
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(self.pull_requests.iter().map(|pr| {
                    let tone = pr.tone.map_or(muted, |tone| hsla(pr_tone(tone, p)));
                    div()
                        .pl(px(12. * pr.depth as f32))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .min_w_0()
                        .child(pr.icon.render(px(12.), tone))
                        .child(div().flex_none().font_family(mono.clone()).child(format!("#{}", pr.number)))
                        .child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(pr.title.clone()))
                        .children(pr.stack.clone().map(|stack| div().ml_auto().flex_none().pl(px(4.)).text_size(px(crate::theme::metrics::TEXT_3XS)).child(stack)))
                }))
        });
        div()
            .debug_selector(|| "thread-hover-card".into())
            .relative()
            .max_w(px(MAX_WIDTH))
            .rounded(px(6.))
            .shadow(toast_shadow())
            .child(crate::glass::backdrop(6., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(
                div()
                    .relative()
                    .max_w(px(MAX_WIDTH))
                    .px(px(12.))
                    .py(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .text_xs()
                    .child(div().debug_selector(|| "thread-hover-title".into()).min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.title.clone()))
                    .child(
                        div()
                            .pl(px(2.))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .text_color(muted)
                            .children(project)
                            .children(environment)
                            .children(branch)
                            .children(mismatch)
                            .children(model)
                            .children(handed_off)
                            .children(terminals)
                            .children(error),
                    )
                    .children(pull_requests),
            )
    }
}

/// `<TooltipProvider delay={150} closeDelay={0} timeout={400}>` timings.
const OPEN_DELAY: Duration = Duration::from_millis(150);
const WARM_TIMEOUT: Duration = Duration::from_millis(400);

struct OpenHover {
    row_id: SharedString,
    card: Entity<ThreadHoverCard>,
}

/// Owns the row-anchored thread hover card and its 150ms open / 0ms leave / 400ms warm window.
#[derive(Default)]
pub(super) struct SidebarHoverState {
    active_row: Option<SharedString>,
    target: Option<HoverTarget>,
    open: Option<OpenHover>,
    warm: bool,
    last_search_query: String,
    open_timer: Option<Task<()>>,
    cooldown_timer: Option<Task<()>>,
    row_bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    visible_rows: Rc<RefCell<HashSet<SharedString>>>,
    tracked_row: Rc<RefCell<Option<SharedString>>>,
    dismiss_scheduled: Rc<RefCell<bool>>,
}

impl SidebarHoverState {
    pub(super) fn sync_search(&mut self, query: &str) {
        if self.last_search_query != query {
            self.last_search_query = query.to_owned();
            self.clear();
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.active_row.is_some() || self.open.is_some()
    }

    pub(super) fn record_row_bounds(&self, row_id: SharedString, pane_id: EntityId) -> AnyElement {
        let bounds_map = self.row_bounds.clone();
        let visible_rows = self.visible_rows.clone();
        let tracked_row = self.tracked_row.clone();
        gpui::canvas(
            move |bounds, _, cx| {
                visible_rows.borrow_mut().insert(row_id.clone());
                let previous = bounds_map.borrow_mut().insert(row_id.clone(), bounds);
                if previous != Some(bounds) && tracked_row.borrow().as_ref() == Some(&row_id) {
                    cx.notify(pane_id);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    pub(super) fn begin_frame_probe(&self) -> AnyElement {
        let visible_rows = self.visible_rows.clone();
        gpui::canvas(
            move |_, _, _| {
                visible_rows.borrow_mut().clear();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size(px(0.))
        .into_any_element()
    }

    pub(super) fn end_frame_probe(&self, pane: WeakEntity<super::SidebarPane>, active_list: ListState) -> AnyElement {
        let bounds_map = self.row_bounds.clone();
        let visible_rows = self.visible_rows.clone();
        let tracked_row = self.tracked_row.clone();
        let dismiss_scheduled = self.dismiss_scheduled.clone();
        gpui::canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                let Some(row_id) = tracked_row.borrow().clone() else {
                    return;
                };
                let viewport = active_list.viewport_bounds();
                let on_screen = visible_rows.borrow().contains(&row_id)
                    && (viewport.size.width <= px(0.)
                        || viewport.size.height <= px(0.)
                        || bounds_map.borrow().get(&row_id).is_some_and(|bounds| bounds.intersects(&viewport)));
                if !on_screen && !*dismiss_scheduled.borrow() {
                    *dismiss_scheduled.borrow_mut() = true;
                    let pane = pane.clone();
                    let dismiss_flag = dismiss_scheduled.clone();
                    window.defer(cx, move |_, cx| {
                        *dismiss_flag.borrow_mut() = false;
                        let _ = pane.update(cx, |this, cx| {
                            this.hover.dismiss(cx);
                        });
                    });
                }
            },
        )
        .absolute()
        .size(px(0.))
        .into_any_element()
    }

    pub(super) fn clear(&mut self) -> bool {
        let changed = self.active_row.is_some()
            || self.target.is_some()
            || self.open.is_some()
            || self.warm
            || self.open_timer.is_some()
            || self.cooldown_timer.is_some();
        self.active_row = None;
        *self.tracked_row.borrow_mut() = None;
        self.target = None;
        self.open = None;
        self.warm = false;
        self.open_timer = None;
        self.cooldown_timer = None;
        changed
    }

    pub(super) fn dismiss(&mut self, cx: &mut Context<super::SidebarPane>) {
        if self.clear() {
            cx.notify();
        }
    }

    pub(super) fn on_hover(
        &mut self,
        suppressed: bool,
        row_id: SharedString,
        target: HoverTarget,
        hovered: bool,
        cx: &mut Context<super::SidebarPane>,
    ) {
        if suppressed {
            self.dismiss(cx);
            return;
        }
        if hovered {
            if self.active_row.as_ref() == Some(&row_id) {
                self.target = Some(target);
                return;
            }
            self.cooldown_timer = None;
            self.open_timer = None;
            self.active_row = Some(row_id.clone());
            *self.tracked_row.borrow_mut() = Some(row_id.clone());
            self.target = Some(target.clone());
            if self.warm {
                let card = cx.new(|cx| ThreadHoverCard::new(&target, cx));
                self.open = Some(OpenHover { row_id, card });
                cx.notify();
            } else {
                self.open = None;
                let expected = row_id;
                self.open_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(OPEN_DELAY).await;
                    let _ = this.update(cx, |pane, cx| {
                        let suppressed = pane.hover_suppressed();
                        pane.hover.finish_open_timer(&expected, suppressed, cx);
                    });
                }));
            }
            return;
        }

        // Ignore stale leave events from an earlier row after a newer row is already active.
        if self.active_row.as_ref() != Some(&row_id) {
            return;
        }
        self.active_row = None;
        *self.tracked_row.borrow_mut() = None;
        self.target = None;
        self.open_timer = None;
        let was_open = self.open.take().is_some();
        if was_open || self.warm {
            self.warm = true;
            self.cooldown_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(WARM_TIMEOUT).await;
                let _ = this.update(cx, |pane, _| {
                    if pane.hover.active_row.is_none() {
                        pane.hover.warm = false;
                    }
                    pane.hover.cooldown_timer = None;
                });
            }));
            if was_open {
                cx.notify();
            }
        }
    }

    fn finish_open_timer(&mut self, expected: &SharedString, suppressed: bool, cx: &mut Context<super::SidebarPane>) {
        self.open_timer = None;
        if suppressed || self.active_row.as_ref() != Some(expected) {
            return;
        }
        let Some(target) = self.target.as_ref() else {
            return;
        };
        self.warm = true;
        self.cooldown_timer = None;
        let card = cx.new(|cx| ThreadHoverCard::new(target, cx));
        self.open = Some(OpenHover { row_id: expected.clone(), card });
        cx.notify();
    }

    pub(super) fn overlay(&mut self, suppressed: bool, active_list: &ListState) -> Option<AnyElement> {
        if suppressed {
            self.clear();
            return None;
        }
        let open = self.open.as_ref()?;
        let row_bounds = self.row_bounds.borrow().get(&open.row_id).copied()?;
        let viewport = active_list.viewport_bounds();
        if viewport.size.width > px(0.) && viewport.size.height > px(0.) && !row_bounds.intersects(&viewport) {
            self.clear();
            return None;
        }
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position_mode(gpui::AnchoredPositionMode::Window)
                    .position(point(row_bounds.right() + px(4.), row_bounds.top()))
                    .snap_to_window_with_margin(px(8.))
                    .child(open.card.clone()),
            )
            .with_priority(140)
            .into_any_element(),
        )
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use serde_json::{Value, json};
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};

    use super::*;
    use crate::context_menu::project_filter_tests::install_state;

    fn provider(instance: &str, name: Option<&str>) -> Value {
        json!({
            "instanceId": instance, "driver": "codex", "displayName": name, "enabled": true, "installed": true, "status": "ready",
            "models": [{"slug": "gpt-5", "name": "GPT-5 Codex", "shortName": "GPT-5"}]
        })
    }

    fn config(label: &str, machine: &str, providers: Vec<Value>) -> t3_core::server_config::ServerConfigState {
        let mut config = t3_core::server_config::ServerConfigState::default();
        config.apply(serde_json::from_value(json!({
            "type": "snapshot",
            "config": {
                "environment": {"environmentId": format!("id-{label}"), "label": label, "platform": {"os": "macos", "arch": "arm64", "machine": machine}, "capabilities": {"projectOrganization": true}},
                "cwd": "/", "settings": {}, "providers": providers
            }
        })).unwrap());
        config
    }

    fn thread_json(id: &str, project: &str, extra: Value) -> Value {
        let mut value = json!({
            "id": id, "projectId": project, "title": format!("Thread {id}"), "createdAt": "2026-01-02T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "providerInstanceId": "codex", "modelSelection": {"instanceId": "codex", "model": "gpt-5"}, "runtimeMode": "auto", "interactionMode": "default",
            "lineage": {"rootThreadId": id}, "status": "idle", "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        value
    }

    fn pull_request(number: u64, head: &str, base: &str, updated: &str) -> Value {
        json!({
            "host": "github.com", "repository": "acme/app", "number": number, "url": format!("https://github.com/acme/app/pull/{number}"),
            "source": "manual", "linkedAt": "2026-01-01T00:00:00Z",
            "snapshot": {"state": "open", "title": format!("Layer {number}"), "headBranch": head, "baseBranch": base, "isDraft": number == 8, "updatedAt": updated}
        })
    }

    /// The next checkout read, past the favicon requests a fleet environment also sends.
    async fn next_checkout_read(fixture: &mut ClientFixture) -> (Value, tokio::sync::oneshot::Sender<Result<Value, t3_client::session::RpcError>>) {
        loop {
            let (method, params, reply) = fixture.next_request().await;
            if method == t3_core::vcs::method::VCS_REFRESH_STATUS {
                return (params, reply);
            }
        }
    }

    fn set_terminals(cx: &mut TestAppContext, thread: &str, ids: &[&str]) {
        cx.update(|cx| {
            let mut running = crate::terminal::RunningTerminals::default();
            running.by_thread.insert(thread.into(), ids.iter().map(|id| (*id).to_owned()).collect());
            cx.set_global(running);
        });
    }

    #[gpui::test]
    async fn local_card_shows_every_web_line_and_compares_the_checkout_once(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            state.server_config = config("Studio", "desktop", vec![provider("codex", None), provider("codex_work", Some("Work"))]);
            state.shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(thread_json("hover-thread", "checkout-a", json!({
                "branch": "feature/hover",
                "providerInstanceId": "codex_work",
                "providerInstanceHistory": ["codex", "codex_work"],
                "modelSelection": {"instanceId": "codex_work", "model": "gpt-5"},
                "latestRunId": "run-1",
                "lastError": "rate limited",
                "lastErrorClass": "usage_limit",
                "pullRequests": [pull_request(7, "base", "main", "2026-01-03T00:00:00Z"), pull_request(8, "top", "base", "2026-01-04T00:00:00Z")]
            }))).unwrap()];
            state.rebuild_sidebar(cx);
        });
        set_terminals(cx, "hover-thread", &["term-1", "term-2"]);
        let favicon = state.read_with(cx, |state, _| ProjectVisual::for_project(state, "checkout-a"));
        let target = HoverTarget { state: state.clone(), key: None, thread_id: "hover-thread".into(), project: Some("Shared repo".into()), favicon, is_repo: None };

        let (card, vcx) = cx.add_window_view(|_, cx| ThreadHoverCard::new(&target, cx));
        let checkout = card.update(vcx, |card, _| card._checkout.take()).expect("local thread with a branch compares its checkout");
        let (method, params, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::vcs::method::VCS_REFRESH_STATUS);
        assert_eq!(params, json!({"cwd": "/fixture/a", "localOnly": true}));
        reply.send(Ok(json!({"isRepo": true, "refName": "main", "hasWorkingTreeChanges": false}))).unwrap();
        checkout.await;
        vcx.update(|window, cx| window.draw(cx).clear());

        card.read_with(vcx, |card, _| {
            assert_eq!(card.title.as_ref(), "Thread hover-thread");
            assert_eq!(card.environment.as_ref().map(|(label, icon)| (label.as_ref(), *icon)), Some(("Studio", Icon::Monitor)));
            assert_eq!(card.model.as_ref().map(|model| model.text.as_ref()), Some("GPT-5 · Work"));
            assert_eq!(card.handed_off.as_deref(), Some("Handed off from Codex"));
            assert_eq!(card.terminals, 2);
            assert_eq!(card.error, Some(true));
            assert!(card.branch_mismatch, "local checkout on main while the thread records feature/hover");
            let lines: Vec<(u64, usize, Option<&str>, Option<PrTone>)> = card.pull_requests.iter().map(|line| (line.number, line.depth, line.stack.as_deref(), line.tone)).collect();
            assert_eq!(lines, vec![(7, 0, Some("chain · 2"), Some(PrTone::Open)), (8, 1, None, Some(PrTone::Draft))]);
        });
        for selector in [
            "thread-hover-title", "thread-hover-project", "thread-hover-environment", "thread-hover-branch", "thread-hover-branch-mismatch",
            "thread-hover-model", "thread-hover-handoff", "thread-hover-terminals", "thread-hover-usage-limit", "thread-hover-pull-requests",
        ] {
            let bounds = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} rendered"));
            assert!(bounds.size.width <= px(MAX_WIDTH), "{selector} stays inside the card");
        }
        assert!(fixture.take_commands().is_empty(), "one checkout read per open card");
        state.update(vcx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    async fn closing_the_card_drops_its_checkout_read(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            state.shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(thread_json("hover-thread", "checkout-a", json!({"branch": "feature/hover"}))).unwrap()];
            state.rebuild_sidebar(cx);
        });
        let target = HoverTarget { state: state.clone(), key: None, thread_id: "hover-thread".into(), project: None, favicon: ProjectVisual::default(), is_repo: None };
        let card = cx.update(|cx| cx.new(|cx| ThreadHoverCard::new(&target, cx)));
        let (_, _, mut reply) = fixture.next_request().await;
        drop(card);
        cx.update(|_| {});
        reply.closed().await;
        state.update(cx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    async fn fleet_card_reads_the_owning_environment_and_keeps_terminals_to_the_current_one(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture_local) = cx.update(|cx| install_state(cx, runtime.handle().clone()));
        let mut fixtures = Vec::new();
        for (saved, label, machine) in [("env-a", "Laptop", "laptop"), ("env-b", "Cloud box", "cloud")] {
            let (client, fixture) = ClientFixture::handle(serde_json::from_value(json!({
                "authenticated": true,
                "permissions": [t3_core::auth::ORCHESTRATION_READ_SCOPE, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE]
            })).unwrap());
            let mut shell = t3_core::shell::ShellState::default();
            shell.apply(serde_json::from_value(json!({
                "kind": "snapshot",
                "snapshot": {
                    "snapshotSequence": 1,
                    "projects": [{"id": format!("proj-{saved}"), "title": saved, "workspaceRoot": format!("/{saved}"), "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"}],
                    "threads": [thread_json("shared-th", &format!("proj-{saved}"), json!({"branch": "feature/x"}))]
                }
            })).unwrap());
            shell.synchronized = true;
            let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config(label, machine, vec![provider("codex", None)]), ConnectionStatus::Connected));
            let _events = runtime.spawn(install).await.unwrap();
            fixtures.push(fixture);
        }
        let mut fixture_b = fixtures.pop().unwrap();
        let mut fixture_a = fixtures.pop().unwrap();
        state.update(cx, |state, cx| {
            state.environments.active = Some("env-a".into());
            state.shell.snapshot.as_mut().unwrap().threads.clear();
            state.rebuild_sidebar(cx);
        });
        cx.run_until_parked();
        set_terminals(cx, "shared-th", &["term-1"]);
        fixture_local.take_commands();

        let key_b = ThreadKey { environment_id: "env-b".into(), thread_id: "shared-th".into() };
        let target_b = HoverTarget { state: state.clone(), key: Some(key_b), thread_id: "shared-th".into(), project: Some("env-b".into()), favicon: ProjectVisual::default(), is_repo: None };
        let (card_b, vcx) = cx.add_window_view(|_, cx| ThreadHoverCard::new(&target_b, cx));
        let checkout = card_b.update(vcx, |card, _| card._checkout.take()).expect("remote thread compares its own checkout");
        let (params, reply) = next_checkout_read(&mut fixture_b).await;
        assert_eq!(params["cwd"], "/env-b");
        reply.send(Ok(json!({"isRepo": true, "refName": "main"}))).unwrap();
        checkout.await;
        card_b.read_with(vcx, |card, _| {
            assert_eq!(card.environment.as_ref().map(|(label, icon)| (label.as_ref(), *icon)), Some(("Cloud box", Icon::Cloud)));
            assert_eq!(card.terminals, 0, "terminal metadata belongs to the current environment only");
            assert!(card.branch_mismatch, "env-b's checkout moved to main");
            assert_eq!(card.model.as_ref().map(|model| model.text.as_ref()), Some("GPT-5"));
        });
        let reads = |commands: Vec<&str>| commands.into_iter().filter(|method| *method == t3_core::vcs::method::VCS_REFRESH_STATUS).count();
        assert_eq!(reads(fixture_a.take_commands()) + reads(fixture_local.take_commands()), 0, "only the owning environment is asked");

        let key_a = ThreadKey { environment_id: "env-a".into(), thread_id: "shared-th".into() };
        let target_a = HoverTarget { key: Some(key_a), project: Some("env-a".into()), ..target_b };
        let card_a = vcx.update(|_, cx| cx.new(|cx| ThreadHoverCard::new(&target_a, cx)));
        let (params, _) = next_checkout_read(&mut fixture_a).await;
        assert_eq!(params["cwd"], "/env-a");
        card_a.read_with(vcx, |card, _| {
            assert_eq!(card.environment.as_ref().map(|(label, _)| label.as_ref()), Some("Laptop"));
            assert_eq!(card.terminals, 1);
        });
        state.update(vcx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    async fn worktree_hover_card_checks_worktree_checkout_and_clears_branch_when_removed(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            state.shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(thread_json("wt-thread", "checkout-a", json!({
                "branch": "feature/removed-wt",
                "worktreePath": "/tmp/removed-worktree",
            }))).unwrap()];
            state.rebuild_sidebar(cx);
        });
        let target = HoverTarget {
            state: state.clone(),
            key: None,
            thread_id: "wt-thread".into(),
            project: Some("Shared repo".into()),
            favicon: ProjectVisual::default(),
            is_repo: None,
        };
        let (card, vcx) = cx.add_window_view(|_, cx| ThreadHoverCard::new(&target, cx));
        let checkout = card.update(vcx, |card, _| card._checkout.take()).expect("worktree thread must verify checkout existence");
        let (method, params, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::vcs::method::VCS_REFRESH_STATUS);
        assert_eq!(params, json!({"cwd": "/tmp/removed-worktree", "localOnly": true}));
        reply.send(Ok(json!({"isRepo": false, "refName": null, "hasWorkingTreeChanges": false}))).unwrap();
        checkout.await;
        vcx.update(|window, cx| window.draw(cx).clear());
        card.read_with(vcx, |card, _| {
            assert_eq!(card.branch, None, "removed worktree checkout must clear branch on hover card");
            assert!(!card.branch_mismatch);
        });
        state.update(vcx, |state, _| state.stop_pump());
    }

    struct HoverHost {
        pane: Entity<super::super::SidebarPane>,
        menu: Entity<crate::context_menu::ContextMenu>,
    }

    impl Render for HoverHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .child(
                    div()
                        .w(px(320.))
                        .h_full()
                        .child(self.pane.clone().cached(gpui::StyleRefinement::default().size_full())),
                )
                .child(div().flex_1().h_full())
                .child(self.menu.clone())
        }
    }

    fn setup_hover_host<'a>(
        cx: &'a mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        threads: Vec<Value>,
    ) -> (Entity<AppState>, Entity<super::super::SidebarPane>, &'a mut gpui::VisualTestContext) {
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::composer::init(cx);
            crate::keymap::init(cx);
            let (state, fixture) = install_state(cx, runtime.handle().clone());
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |s, cx| {
                s.sidebar.snapshot_fixture = true;
                s.shell.snapshot.as_mut().unwrap().threads =
                    threads.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect();
                s.rebuild_sidebar(cx);
            });
            (state, fixture)
        });
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| super::super::SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            HoverHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        (state, pane, vcx)
    }

    #[gpui::test]
    async fn row_hover_card_anchors_to_right_edge_regardless_of_pointer_x_and_hands_off_warmly(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, pane, vcx) = setup_hover_host(
            cx,
            &runtime,
            vec![
                thread_json("th-1", "checkout-a", json!({ "title": "First thread", "pinnedAt": "2026-01-02T00:00:00Z" })),
                thread_json("th-2", "checkout-a", json!({ "title": "Second thread", "pinnedAt": "2026-01-01T23:00:00Z", "updatedAt": "2026-01-01T23:00:00Z" })),
            ],
        );

        let row1_bounds = vcx.debug_bounds("thread-row-None-th-1").expect("row 1 bounds");
        let row2_bounds = vcx.debug_bounds("thread-row-None-th-2").expect("row 2 bounds");

        // 1. Cold hover near left edge waits 150ms, then anchors at (row1.right() + 4, row1.top()).
        vcx.update(|window, cx| {
            window.simulate_mouse_move(point(row1_bounds.left() + px(14.), row1_bounds.center().y), cx);
        });
        vcx.run_until_parked();
        vcx.executor().advance_clock(Duration::from_millis(100));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-hover-card").is_none(), "hover card must wait for 150ms cold delay");

        vcx.executor().advance_clock(Duration::from_millis(60));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let left_card = vcx.debug_bounds("thread-hover-card").expect("hover card opens after 150ms");
        assert!((left_card.left() - (row1_bounds.right() + px(4.))).abs() <= px(1.));
        assert!((left_card.top() - row1_bounds.top()).abs() <= px(1.));

        // 2. Leave (closeDelay = 0), wait >400ms cooldown, then hover near right edge -> identical card bounds.
        vcx.update(|window, cx| {
            window.simulate_mouse_move(point(px(900.), px(600.)), cx);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-hover-card").is_none(), "leaving row must dismiss immediately");

        vcx.executor().advance_clock(Duration::from_millis(450));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.simulate_mouse_move(point(row1_bounds.right() - px(14.), row1_bounds.center().y), cx);
        });
        vcx.run_until_parked();
        vcx.executor().advance_clock(Duration::from_millis(160));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let right_card = vcx.debug_bounds("thread-hover-card").expect("hover card opens on right edge");
        assert_eq!(right_card, left_card, "hover card bounds must not depend on horizontal pointer position");

        // 3. Moving to a second row while warm switches immediately (0ms delay) and anchors to row 2.
        vcx.update(|window, cx| {
            window.simulate_mouse_move(row2_bounds.center(), cx);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let row2_card = vcx.debug_bounds("thread-hover-card").expect("warm handoff must open row 2 immediately");
        assert!((row2_card.left() - (row2_bounds.right() + px(4.))).abs() <= px(1.));
        assert!((row2_card.top() - row2_bounds.top()).abs() <= px(1.));

        // 4. Stale leave for row 1 arriving after row 2 entered must not dismiss row 2; clicking row 2 dismisses.
        let target1 = HoverTarget {
            state: state.clone(),
            key: None,
            thread_id: "th-1".into(),
            project: Some("Shared repo".into()),
            favicon: ProjectVisual::default(),
            is_repo: None,
        };
        pane.update(vcx, |pane, cx| {
            pane.hover.on_hover(false, "thread-row-None-th-1".into(), target1, false, cx);
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-hover-card").is_some(), "stale leave for row 1 must not close row 2");

        vcx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button: gpui::MouseButton::Left,
                    position: row2_bounds.center(),
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-hover-card").is_none(), "mouse-down on row must dismiss hover card");

        state.update(vcx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    async fn search_result_row_hover_card_anchors_to_right_edge(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, pane, vcx) = setup_hover_host(
            cx,
            &runtime,
            vec![
                thread_json("sr-1", "checkout-a", json!({ "title": "Search Target Alpha" })),
                thread_json("sr-2", "checkout-a", json!({ "title": "Unrelated Beta" })),
            ],
        );

        pane.update(vcx, |pane, cx| {
            pane.search.set_query("Target".into(), &pane.state, cx);
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let key = ThreadKey::new("", "sr-1");
        let search_selector: &'static str = Box::leak(format!("search-result-{:?}", key).into_boxed_str());
        let search_bounds = vcx.debug_bounds(search_selector).expect("search result row bounds");

        vcx.update(|window, cx| {
            window.simulate_mouse_move(search_bounds.center(), cx);
        });
        vcx.run_until_parked();
        vcx.executor().advance_clock(Duration::from_millis(160));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let card_bounds = vcx.debug_bounds("thread-hover-card").expect("search result hover card must open");
        assert!((card_bounds.left() - (search_bounds.right() + px(4.))).abs() <= px(1.));
        assert!((card_bounds.top() - search_bounds.top()).abs() <= px(1.));

        // Updating search query clears active hover card immediately.
        pane.update(vcx, |pane, cx| {
            pane.search.set_query("Beta".into(), &pane.state, cx);
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-hover-card").is_none(), "changing search query must dismiss hover card");

        state.update(vcx, |state, _| state.stop_pump());
    }
}

/// The pull request badge's hover affordance, built only once the badge is hovered.
pub(super) struct PrMiniList(pub(super) Vec<t3_core::pull_requests::link::ThreadLink>);

impl Render for PrMiniList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::{ThemeSlot, ThemeSlotExt as _, theme_slot_clipped_background_layer};
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        div()
            .relative()
            .w(px(MAX_WIDTH))
            .rounded(px(8.))
            .shadow(toast_shadow())
            .theme_slot(ThemeSlot::PopoverPopup, cx)
            .child(theme_slot_clipped_background_layer(ThemeSlot::PopoverPopup, px(8.), cx)
                .unwrap_or_else(|| crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx).into_any_element()))
            .child(div()
                .id("sidebar-pr-mini-list")
                .relative()
                .max_h(px(320.))
                .overflow_y_scroll()
                .p(px(4.))
                .text_xs()
                .children(model::pull_request_list_lines(&self.0).into_iter().enumerate().map(|(index, line)| {
                    let badge = line.link.snapshot.as_ref().map(|snapshot| BadgeState::of(snapshot.state, snapshot.is_draft));
                    let icon = pr_icon(badge.map_or(PrGlyph::PullRequest, BadgeState::glyph));
                    let tone = badge.map_or(muted, |badge| hsla(pr_tone(badge.tone(), p)));
                    let title = line.link.snapshot.as_ref().map_or(&line.link.repository, |snapshot| &snapshot.title);
                    let url = line.link.url.clone();
                    div()
                        .id(("linked-pr", index))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .pl(px(6. + 12. * line.depth.min(MAX_PR_DEPTH) as f32))
                        .pr(px(6.))
                        .h(px(28.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|row| row.bg(hsla(p.accent)))
                        .child(icon.render(px(12.), tone))
                        .child(div().flex_none().text_color(muted).child(format!("#{}", line.link.number)))
                        .child(div().min_w_0().truncate().text_color(hsla(p.popover_foreground)).child(title.clone()))
                        .children(line.stack_label().map(|label| div().ml_auto().flex_none().text_color(muted).child(label)))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })))
    }
}
