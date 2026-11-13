//! Thread hover card (`SidebarThreadTooltip` in `sidebar/SidebarThreadRow.tsx`, `ThreadHoverCard.tsx`).
//! Built when the glass tooltip opens, from what the app already holds, and dropped with it; the
//! one read it makes, the checkout's branch, goes with it.

use gpui::{AnyElement, App, AppContext as _, BoxShadow, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div, point, prelude::*, px, rgba};
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

/// Lets a running read go when the card closes.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

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
    Read { compared: SidebarThread, read: AbortOnDrop<Option<String>> },
}

impl ThreadHoverCard {
    pub(super) fn new(target: &HoverTarget, cx: &mut Context<Self>) -> Self {
        let entity = target.state.clone();
        let state = entity.read(cx);
        let Some(thread) = find_thread(state, target) else {
            return Self::empty(target);
        };
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
            Some(Checkout::Read { compared, mut read }) => {
                card._checkout = Some(cx.spawn(async move |this, cx| {
                    let current = (&mut read.0).await.ok().flatten();
                    let _ = this.update(cx, |card, cx| {
                        card.branch_mismatch = model::branch_mismatch(&compared, current.as_deref());
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

    /// Reads the project checkout's branch once, for a local-mode thread that records one, from
    /// the environment that owns the thread.
    fn checkout(target: &HoverTarget, state: &AppState, thread: &SidebarThread, cx: &App) -> Option<Checkout> {
        let root = match &target.key {
            Some(key) => state
                .scoped_project(&t3_core::sidebar::grouping::ProjectRef { environment_id: key.environment_id.clone(), project_id: thread.project_id.clone() })
                .map(|project| project.project.workspace_root.as_str()),
            None => state.shell.project(&thread.project_id).map(|project| project.workspace_root.as_str()),
        };
        let cwd = model::checkout_to_compare(thread, root)?.to_owned();
        #[cfg(feature = "snapshot")]
        if state.sidebar.snapshot_fixture {
            let current = cx.try_global::<crate::snapshot::CheckoutFixture>().and_then(|fixture| fixture.0.get(&cwd).cloned());
            return Some(Checkout::Known(model::branch_mismatch(thread, current.as_deref())));
        }
        #[cfg(not(feature = "snapshot"))]
        let _ = cx;
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
            serde_json::from_value::<t3_core::vcs::VcsLocalStatus>(value).ok()?.ref_name
        }));
        Some(Checkout::Read { compared: thread.clone(), read })
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
                .child(div().flex_1().min_w_0().child("You're currently checked out on another branch."))
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
                .child(div().min_w_0().truncate().child(if usage_limit { "Usage limit reached" } else { "Error occurred" }))
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

/// The glass tooltip's builder: the card exists only while it shows.
pub(super) fn tooltip(target: HoverTarget) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    move |_, cx| cx.new(|cx| ThreadHoverCard::new(&target, cx)).into()
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
        let target = HoverTarget { state: state.clone(), key: None, thread_id: "hover-thread".into(), project: Some("Shared repo".into()), favicon };

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
        let target = HoverTarget { state: state.clone(), key: None, thread_id: "hover-thread".into(), project: None, favicon: ProjectVisual::default() };
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
        let target_b = HoverTarget { state: state.clone(), key: Some(key_b), thread_id: "shared-th".into(), project: Some("env-b".into()), favicon: ProjectVisual::default() };
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
}
