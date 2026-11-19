//! Incoming links: `NewThreadLinkView.tsx`, `ThreadLinkPrompt.tsx` and
//! `DesktopWebLinkCoordinator.tsx`. A URL only stages a draft; it never sends a turn.
use crate::{
    app_state::{AppEvent, AppState},
    composer::Composer,
    settings::controls::{Button, ButtonSize, ButtonVariant},
    theme::{hsla, palette},
    toast::{Toast, ToastKind},
};
use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::prompt_links::{self as rules, NewThreadLink};

impl AppState {
    pub(crate) fn open_prompt_url(&mut self, url: String, cx: &mut Context<Self>) {
        if let Some(environment)=t3_core::settings::providers::chatgpt::resume_environment(&url) {
            cx.defer(move |cx|crate::onboarding::resume(environment,cx));
            return;
        }
        self.prompt_link_urls.push_back(url);
        cx.emit(AppEvent::PromptLink);
    }
}

const PR_LINK_CHECKOUT_TOAST_KEY: &str = "pr-link-checkout";

pub struct PromptLinks {
    state: Entity<AppState>,
    composer: Entity<Composer>,
    pending: Option<String>,
    /// Deferred draft/thread target waiting for a saved environment switch to finish.
    pending_target: Option<PendingTarget>,
    page: Option<(String, String)>,
    fallback: Option<NewThreadLink>,
    /// Repository missing on all connected environments, eligible for `"Clone repository"`.
    missing_repository: Option<rules::Repository>,
    /// Unlinked PR confirmation & checkout handoff (`PullRequestLinkCheckout` in `NewThreadLinkView.tsx`).
    pr_checkout: Option<PendingPrCheckout>,
    scratch: bool,
    cloning: bool,
    sequence: u64,
    connecting: Option<String>,
    _subscription: Subscription,
}

#[derive(Clone)]
struct PendingPrCheckout {
    saved_id: String,
    project_id: String,
    workspace_root: String,
    pull_request: rules::PullRequest,
    link: NewThreadLink,
    checking_out: bool,
}

enum PendingTarget {
    Thread {
        saved_id: String,
        environment_id: String,
        thread_id: String,
        prompt: Option<rules::LinkedPrompt>,
    },
    Draft {
        saved_id: String,
        project_id: String,
        link: NewThreadLink,
    },
    PrCheckout(PendingPrCheckout),
}

impl PromptLinks {
    pub fn new(state: Entity<AppState>, composer: Entity<Composer>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
            if matches!(event, AppEvent::PromptLink | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase) {
                this.process(window, cx);
            }
        });
        let mut this = Self {
            state,
            composer,
            pending: None,
            pending_target: None,
            page: None,
            fallback: None,
            missing_repository: None,
            pr_checkout: None,
            scratch: false,
            cloning: false,
            sequence: 0,
            connecting: None,
            _subscription: subscription,
        };
        this.process(window, cx);
        this
    }
    fn fail(&mut self, title: &str, description: String, cx: &mut Context<Self>) {
        self.page = Some((title.into(), description));
        cx.notify();
    }
    fn process(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.state.update(cx, |s, _| s.prompt_link_urls.pop_front()) {
            self.sequence += 1;
            self.connecting = None;
            self.pending_target = None;
            self.scratch = false;
            self.cloning = false;
            self.missing_repository = None;
            self.pr_checkout = None;
            self.pending = Some(url);
            self.page = Some(("Opening the link...".into(), String::new()));
            self.fallback = None;
            cx.notify();
        }
        // Complete any cross-environment target once the destination environment is active and ready.
        if let Some(target) = self.pending_target.take() {
            let (saved_id, ready) = {
                let app = self.state.read(cx);
                let active_saved = app.environments.active.as_deref();
                let ready = app.shell.synchronized && app.shell.snapshot.is_some() && app.server_config.config.is_some();
                match &target {
                    PendingTarget::Thread { saved_id, .. } | PendingTarget::Draft { saved_id, .. } => {
                        (saved_id.clone(), active_saved == Some(saved_id.as_str()) && ready)
                    }
                    PendingTarget::PrCheckout(checkout) => {
                        (checkout.saved_id.clone(), active_saved == Some(checkout.saved_id.as_str()) && ready)
                    }
                }
            };
            if !ready {
                if self.connecting.as_deref() != Some(&saved_id) {
                    self.connecting = Some(saved_id.clone());
                    self.state.update(cx, |s, cx| s.connect_environment(&saved_id, cx));
                }
                self.pending_target = Some(target);
                return;
            }
            self.connecting = None;
            match target {
                PendingTarget::Thread { environment_id, thread_id, prompt, .. } => {
                    self.open_thread(&environment_id, &thread_id, prompt.as_ref(), window, cx);
                }
                PendingTarget::Draft { project_id, link, .. } => {
                    self.open_draft(&project_id, link, window, cx);
                }
                PendingTarget::PrCheckout(checkout) => {
                    self.run_pr_checkout(checkout, window, cx);
                }
            }
            cx.notify();
            return;
        }
        let Some(url) = self.pending.clone() else { return };
        let Some(path) = rules::prompt_link_app_path(&url) else {
            self.pending = None;
            return self.fail("Could not open the link", "This is not a T3 Code prompt link.".into(), cx);
        };
        let (route, query) = path.split_once('?').unwrap_or((&path, ""));
        if route != "/new" {
            let Some((env, thread)) = route.trim_start_matches('/').split_once('/') else {
                return;
            };
            let app = self.state.read(cx);
            let current = app.active_environment().and_then(|e| e.environment_id.as_deref());
            let saved = app
                .environments
                .environments
                .iter()
                .find(|e| e.environment_id.as_deref() == Some(env))
                .map(|e| e.id.as_str());
            let ready = app.shell.synchronized && app.shell.snapshot.is_some() && app.server_config.config.is_some();
            match rules::thread_link_route(env, current, saved, ready) {
                rules::ThreadLinkRoute::Connect(id) => {
                    let id = id.to_owned();
                    if self.connecting.as_deref() != Some(&id) {
                        self.connecting = Some(id.clone());
                        self.state.update(cx, |s, cx| s.connect_environment(&id, cx));
                    }
                    return;
                }
                rules::ThreadLinkRoute::Wait => return,
                rules::ThreadLinkRoute::MissingEnvironment => {
                    self.pending = None;
                    return self.fail("Could not open the link", "Connect to the machine named by this link first.".into(), cx);
                }
                rules::ThreadLinkRoute::Select => {}
            }
            self.pending = None;
            let parsed = rules::parse_thread_link_prompt(query);
            self.open_thread(env, thread, parsed.as_ref().ok().and_then(|p| p.as_ref()), window, cx);
            if let Err(error) = parsed {
                self.state.update(cx, |s, cx| {
                    s.show_toast(Toast::new(ToastKind::Error, "Could not open the linked prompt").description(error), cx)
                });
            }
            cx.notify();
            return;
        }
        let app = self.state.read(cx);
        if !app.shell.synchronized || app.shell.snapshot.is_none() || app.server_config.config.is_none() {
            return;
        }
        self.pending = None;
        let link = match rules::parse_new_thread_link(query) {
            Ok(link) => link,
            Err(error) => return self.fail("Could not open the link", error, cx),
        };
        let snapshot = app.shell.snapshot.as_ref().expect("ready shell");
        if let NewThreadLink::PullRequest { pull_request, .. } = &link {
            // 1. Check active environment's shell first.
            if let Some(thread) = rules::find_prompt_link_pull_request_thread(&snapshot.threads, pull_request) {
                let id = thread.id.clone();
                let environment = app.active_environment().and_then(|e| e.environment_id.clone()).unwrap_or_default();
                self.open_thread(&environment, &id, link.prompt(), window, cx);
                cx.notify();
                return;
            }
            // 2. Search other connected saved environments in the fleet (`#111`).
            let pr_repo = format!("{}/{}", pull_request.repository.owner, pull_request.repository.name);
            let fleet_pr_match = app
                .fleet
                .environments
                .iter()
                .filter(|env| env.summary.synchronized)
                .flat_map(|env| {
                    let saved_id = env.summary.owner.saved_id.clone();
                    let env_id = env
                        .config
                        .config
                        .as_ref()
                        .map(|c| c.environment.environment_id.clone())
                        .unwrap_or_else(|| saved_id.clone());
                    let pr_repo = pr_repo.clone();
                    env.summary.threads.iter().filter_map(move |t| {
                        let matched = t.lineage.side_chat != Some(true)
                            && t.thread.pull_requests.iter().any(|l| {
                                l.source != "stack-dismissed"
                                    && l.host.eq_ignore_ascii_case("github.com")
                                    && l.repository.eq_ignore_ascii_case(&pr_repo)
                                    && l.number == pull_request.number
                            });
                        matched.then(|| (saved_id.clone(), env_id.clone(), t.thread.id.clone(), t.thread.updated_at.unwrap_or(0)))
                    })
                })
                .max_by_key(|(_, _, _, updated_at)| *updated_at);
            if let Some((saved_id, environment_id, thread_id, _)) = fleet_pr_match {
                self.pending_target = Some(PendingTarget::Thread {
                    saved_id,
                    environment_id,
                    thread_id,
                    prompt: link.prompt().cloned(),
                });
                self.process(window, cx);
                return;
            }
        }
        if let Some(repo) = link.repository() {
            // Collect matching projects across active environment AND all live fleet environments.
            let active_saved = app.environments.active.clone().unwrap_or_default();
            let active_candidates = rules::find_prompt_link_projects(
                snapshot.projects.iter().filter(|p| p.archived_at.is_none() && !app.is_scratch_project(&p.id)),
                repo,
            );
            let active_owned: Vec<_> = active_candidates.into_iter().cloned().collect();
            let active_best = t3_core::home::most_recent_project(&active_owned, &snapshot.threads).map(|p| p.id.clone());

            let mut lookup_targets: Vec<(String, String, String, String)> = Vec::new();
            let active_env_id = app
                .active_environment()
                .and_then(|e| e.environment_id.clone())
                .unwrap_or_else(|| active_saved.clone());
            // Put the most recently active project first so an unlinked PR checkout targets it.
            if let Some(best_id) = active_best.as_deref()
                && let Some(best_proj) = active_owned.iter().find(|p| p.id == best_id)
            {
                lookup_targets.push((
                    active_saved.clone(),
                    active_env_id.clone(),
                    best_proj.id.clone(),
                    best_proj.workspace_root.clone(),
                ));
            }
            for proj in &active_owned {
                if active_best.as_deref() == Some(proj.id.as_str()) {
                    continue;
                }
                lookup_targets.push((
                    active_saved.clone(),
                    active_env_id.clone(),
                    proj.id.clone(),
                    proj.workspace_root.clone(),
                ));
            }
            let mut fleet_best: Option<(String, String)> = None;
            for env in app.fleet.environments.iter().filter(|e| e.summary.synchronized) {
                let saved_id = &env.summary.owner.saved_id;
                if *saved_id == active_saved {
                    continue;
                }
                let scratch_root = env.config.config.as_ref().and_then(|c| c.scratch_workspace_root.as_deref());
                let matched: Vec<_> = rules::find_prompt_link_projects(
                    env.summary.projects.iter().map(|sp| &sp.project).filter(|p| {
                        p.archived_at.is_none() && scratch_root.is_none_or(|sr| p.workspace_root != sr)
                    }),
                    repo,
                )
                .into_iter()
                .map(|p| (p.id.clone(), p.workspace_root.clone()))
                .collect();
                let env_id = env
                    .config
                    .config
                    .as_ref()
                    .map(|c| c.environment.environment_id.clone())
                    .unwrap_or_else(|| saved_id.clone());
                for (project_id, workspace_root) in &matched {
                    lookup_targets.push((saved_id.clone(), env_id.clone(), project_id.clone(), workspace_root.clone()));
                }
                if fleet_best.is_none()
                    && let Some((first_id, _)) = matched.into_iter().next()
                {
                    fleet_best = Some((saved_id.clone(), first_id));
                }
            }

            if let NewThreadLink::PullRequest { pull_request, .. } = &link
                && !lookup_targets.is_empty()
            {
                // 3. Query `pullRequests.linkedThreads` across every connected environment holding
                // the repository so older-settled PR threads omitted from the shell are found.
                self.lookup_pr_threads(lookup_targets, pull_request.clone(), link, window, cx);
                return;
            }

            if let Some(project) = active_best {
                self.open_draft(&project, link, window, cx);
            } else if let Some((saved_id, project_id)) = fleet_best {
                self.pending_target = Some(PendingTarget::Draft { saved_id, project_id, link });
                self.process(window, cx);
                return;
            } else {
                let missing = repo.clone();
                self.fail(
                    &format!("You don't have {}/{}", repo.owner, repo.name),
                    "Clone it to work on it here, or start without a project.".into(),
                    cx,
                );
                self.missing_repository = Some(missing);
                self.fallback = Some(link);
            }
        } else {
            self.open_scratch(link, window, cx);
        }
        cx.notify();
    }

    fn lookup_pr_threads(
        &mut self,
        targets: Vec<(String, String, String, String)>,
        pull_request: rules::PullRequest,
        link: NewThreadLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sequence = self.sequence;
        let checkout_target = targets.first().map(|(saved_id, _, project_id, workspace_root)| {
            (saved_id.clone(), project_id.clone(), workspace_root.clone())
        });
        let active_saved = self.state.read(cx).environments.active.clone().unwrap_or_default();
        let active_client = self.state.read(cx).client_handle();
        let client_futures: Vec<_> = {
            let app = self.state.read(cx);
            targets
                .into_iter()
                .map(|(saved_id, env_id, project_id, _)| {
                    let fut = (saved_id != active_saved).then(|| app.connections.client(&saved_id));
                    (saved_id, env_id, project_id, fut)
                })
                .collect()
        };
        self.page = Some(("Opening the link...".into(), String::new()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut lookups = Vec::with_capacity(client_futures.len());
            let mut env_to_saved = std::collections::HashMap::new();
            for (saved_id, env_id, project_id, fut) in client_futures {
                env_to_saved.insert(env_id.clone(), saved_id.clone());
                let client = match fut {
                    Some(fut) => fut.await,
                    None => active_client.clone(),
                };
                let Some(client) = client else {
                    lookups.push(rules::PullRequestLookup::Failed);
                    continue;
                };
                let payload = rules::pull_request_linked_threads_input(&project_id, &pull_request);
                match client.request(t3_core::pull_requests::link::LINKED_THREADS, payload).await {
                    Ok(value) => {
                        let threads = value
                            .get("threads")
                            .cloned()
                            .and_then(|v| serde_json::from_value::<Vec<rules::LinkedThread>>(v).ok())
                            .unwrap_or_default();
                        lookups.push(rules::PullRequestLookup::Answer { environment: env_id, threads });
                    }
                    Err(_) => lookups.push(rules::PullRequestLookup::Failed),
                }
            }
            let picked = rules::pick_prompt_link_pull_request_thread(&lookups)
                .flatten()
                .map(|(env, thread)| (env.to_owned(), thread.to_owned()));
            let _ = this.update_in(cx, |this, window, cx| {
                if this.sequence != sequence {
                    return;
                }
                if let Some((env_id, thread_id)) = picked {
                    let saved_id = env_to_saved.get(&env_id).cloned().unwrap_or_else(|| env_id.clone());
                    let is_active = this.state.read(cx).environments.active.as_deref() == Some(saved_id.as_str());
                    if is_active {
                        this.open_thread(&env_id, &thread_id, link.prompt(), window, cx);
                    } else {
                        this.pending_target = Some(PendingTarget::Thread {
                            saved_id,
                            environment_id: env_id,
                            thread_id,
                            prompt: link.prompt().cloned(),
                        });
                        this.process(window, cx);
                    }
                } else if let Some((saved_id, project_id, workspace_root)) = checkout_target {
                    // `PullRequestLinkCheckout`: warn once about hidden characters and require
                    // explicit user confirmation before checking out PR code / running setup.
                    if let Some(prompt) = link.prompt() {
                        crate::composer::prompt_links::warn(&this.state, prompt, cx);
                    }
                    let title = rules::pull_request_checkout_title(&pull_request);
                    this.page = Some((title, rules::PULL_REQUEST_CHECKOUT_DESCRIPTION.into()));
                    this.pr_checkout = Some(PendingPrCheckout {
                        saved_id,
                        project_id,
                        workspace_root,
                        pull_request,
                        link,
                        checking_out: false,
                    });
                    this.fallback = None;
                } else {
                    this.fail(
                        "Could not open the pull request",
                        "No local project matches this pull request.".into(),
                        cx,
                    );
                    this.fallback = Some(link);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_pr_checkout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(checkout) = self.pr_checkout.as_mut() else { return };
        if checkout.checking_out {
            return;
        }
        checkout.checking_out = true;
        let checkout = checkout.clone();
        let active_saved = self.state.read(cx).environments.active.clone().unwrap_or_default();
        if !checkout.saved_id.is_empty() && checkout.saved_id != active_saved {
            self.pending_target = Some(PendingTarget::PrCheckout(checkout));
            self.process(window, cx);
            return;
        }
        self.run_pr_checkout(checkout, window, cx);
    }

    fn run_pr_checkout(&mut self, checkout: PendingPrCheckout, window: &mut Window, cx: &mut Context<Self>) {
        let sequence = self.sequence;
        let thread_id = crate::sidebar::controller::uuid();
        self.state.update(cx, |state, cx| {
            state.show_toast(
                Toast::new(ToastKind::Info, "Preparing the pull request checkout...")
                    .key(PR_LINK_CHECKOUT_TOAST_KEY)
                    .persistent(),
                cx,
            );
        });
        let prepare_payload = t3_core::vcs::prepare_pull_request_thread(
            &checkout.workspace_root,
            &checkout.pull_request.url(),
            t3_core::vcs::PullRequestCheckoutMode::Worktree,
            Some(&thread_id),
        );
        let prepare_req = self
            .state
            .read(cx)
            .rpc(t3_core::vcs::method::GIT_PREPARE_PULL_REQUEST_THREAD, prepare_payload);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let prepared = prepare_req
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<t3_core::vcs::PreparePullRequestThreadResult>(value)
                        .map_err(|error| error.to_string())
                });
            let prepared = match prepared {
                Ok(result) => result,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.sequence != sequence {
                            return;
                        }
                        if let Some(pending) = this.pr_checkout.as_mut() {
                            pending.checking_out = false;
                        }
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                Toast::new(ToastKind::Error, "Could not prepare the pull request checkout")
                                    .description(error)
                                    .key(PR_LINK_CHECKOUT_TOAST_KEY),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                    return;
                }
            };

            let dispatch_plan = this.read_with(cx, |this, cx| {
                if this.sequence != sequence {
                    return Err(None);
                }
                let app = this.state.read(cx);
                let project_default = app
                    .shell
                    .project(&checkout.project_id)
                    .and_then(|project| project.default_model_selection.as_ref());
                let providers = app
                    .server_config
                    .config
                    .as_ref()
                    .map(|config| config.providers.as_slice())
                    .unwrap_or(&[]);
                let Some(model_selection) = rules::default_model_selection_for_project(project_default, providers) else {
                    return Err(Some("No ready provider is configured to open the checked-out thread.".to_owned()));
                };
                let title = rules::pull_request_checkout_thread_title(
                    &checkout.pull_request,
                    Some(prepared.pull_request.title.as_str()),
                );
                let create_cmd = t3_core::commands::create_thread(
                    &crate::sidebar::controller::uuid(),
                    &thread_id,
                    &checkout.project_id,
                    &title,
                    &model_selection,
                    t3_core::server_config::RuntimeMode::FullAccess,
                    t3_core::server_config::InteractionMode::Default,
                    Some(&prepared.branch),
                    prepared.worktree_path.as_deref(),
                );
                let pr_link = rules::pull_request_checkout_link(&checkout.pull_request, &prepared.pull_request.url);
                let link_cmd = t3_core::pull_requests::link::link_command(
                    &crate::sidebar::controller::uuid(),
                    &thread_id,
                    &pr_link,
                );
                let create_rpc = app.rpc(t3_core::commands::method::DISPATCH_COMMAND, create_cmd);
                let link_rpc = app.rpc(t3_core::commands::method::DISPATCH_COMMAND, link_cmd);
                Ok((create_rpc, link_rpc))
            });

            let (create_rpc, link_rpc) = match dispatch_plan {
                Ok(Ok(pair)) => pair,
                Ok(Err(Some(reason))) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pending) = this.pr_checkout.as_mut() {
                            pending.checking_out = false;
                        }
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                Toast::new(ToastKind::Error, "Could not open a thread for the checkout")
                                    .description(reason)
                                    .key(PR_LINK_CHECKOUT_TOAST_KEY),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                    return;
                }
                _ => return,
            };

            if let Err(error) = create_rpc.await {
                let _ = this.update(cx, |this, cx| {
                    if this.sequence != sequence {
                        return;
                    }
                    if let Some(pending) = this.pr_checkout.as_mut() {
                        pending.checking_out = false;
                    }
                    this.state.update(cx, |state, cx| {
                        state.show_toast(
                            Toast::new(ToastKind::Error, "Could not open a thread for the checkout")
                                .description(error.to_string())
                                .key(PR_LINK_CHECKOUT_TOAST_KEY),
                            cx,
                        );
                    });
                    cx.notify();
                });
                return;
            }
            let _ = link_rpc.await;

            let _ = this.update_in(cx, |this, window, cx| {
                if this.sequence != sequence {
                    return;
                }
                this.pr_checkout = None;
                this.page = None;
                this.fallback = None;
                this.state.update(cx, |state, cx| {
                    state.select_thread(&thread_id, cx);
                });
                let has_prompt = checkout.link.prompt().is_some();
                if let Some(prompt) = checkout.link.prompt() {
                    // Hidden-character warning was already shown when the checkout page opened.
                    let cleaned = rules::LinkedPrompt {
                        text: prompt.text.clone(),
                        hidden_characters: 0,
                    };
                    this.composer.update(cx, |composer, cx| {
                        composer.place_linked_prompt(cleaned, vec![], window, cx);
                    });
                }
                let (is_success, title, description) =
                    rules::pull_request_checkout_toast(prepared.is_on_pull_request_head, has_prompt);
                this.state.update(cx, |state, cx| {
                    state.show_toast(
                        Toast::new(if is_success { ToastKind::Success } else { ToastKind::Info }, title)
                            .description(description)
                            .key(PR_LINK_CHECKOUT_TOAST_KEY),
                        cx,
                    );
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn clone_missing_repository(&mut self, repository: rules::Repository, link: NewThreadLink, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloning {
            return;
        }
        let (new_projects_root, base_dir) = {
            let app = self.state.read(cx);
            let cfg = app.server_config.config.as_ref();
            (
                cfg.and_then(|c| c.new_projects_root.clone()),
                cfg.map(|c| c.settings.threads.add_project_base_directory.clone())
                    .filter(|s| !s.trim().is_empty()),
            )
        };
        let Some(new_projects_root) = new_projects_root else {
            self.fail(
                "Could not clone repository",
                "This machine cannot clone from a link. Update T3 Code on it, or clone the repository from the command palette.".into(),
                cx,
            );
            self.fallback = Some(link);
            return;
        };
        self.cloning = true;
        self.page = Some(("Cloning repository...".into(), format!("{}/{}", repository.owner, repository.name)));
        cx.notify();
        let sequence = self.sequence;
        let ghq_layout = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|c| c.settings.threads.ghq_clone_layout);
        let remote_url = format!("https://github.com/{}/{}.git", repository.owner, repository.name);
        let title = repository.name.clone();
        let root = if ghq_layout {
            t3_core::projects::add_project_initial_query(base_dir.as_deref())
        } else {
            new_projects_root
        };
        let dir = t3_core::projects::clone_directory_path(&remote_url, ghq_layout, None);
        let destination = t3_core::projects::clone_destination_path(&root, &dir);
        let project_id = crate::sidebar::controller::uuid();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let payload = t3_core::projects::start_project_clone(&project_id, &title, &created_at, &remote_url, &destination);
        let req = self.state.read(cx).rpc(t3_core::projects::PROJECT_CLONE_START, payload);
        cx.spawn_in(window, async move |this, cx| {
            let result = req
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::from_value::<t3_core::projects::ProjectCloneStartResult>(v).map_err(|e| e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                if this.sequence != sequence {
                    return;
                }
                this.cloning = false;
                match result {
                    Ok(started) => {
                        this.missing_repository = None;
                        if matches!(link, NewThreadLink::PullRequest { .. }) {
                            this.fail(
                                &format!("Cloning {}/{}", repository.owner, repository.name),
                                "Open this link again when the clone finishes.".into(),
                                cx,
                            );
                        } else {
                            this.open_draft(&started.project_id, link, window, cx);
                        }
                    }
                    Err(error) => {
                        this.state.update(cx, |s, cx| {
                            s.show_toast(Toast::new(ToastKind::Error, "Clone failed").description(error), cx);
                        });
                        this.fail(
                            &format!("You don't have {}/{}", repository.owner, repository.name),
                            "Clone it to work on it here, or start without a project.".into(),
                            cx,
                        );
                        this.missing_repository = Some(repository);
                        this.fallback = Some(link);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn open_thread(&mut self, environment: &str, thread: &str, prompt: Option<&rules::LinkedPrompt>, window: &mut Window, cx: &mut Context<Self>) {
        use t3_core::sidebar::tag_filter::ThreadKey;
        self.state.update(cx, |s, cx| s.select_thread(thread, cx));
        let app = self.state.read(cx);
        let selected = app
            .active_environment()
            .and_then(|e| e.environment_id.as_deref())
            .zip(app.selected_thread.as_deref())
            .map(|(env, id)| ThreadKey::new(env, id));
        match rules::prompt_for_selected_thread(&ThreadKey::new(environment, thread), selected.as_ref(), prompt) {
            Ok(prompt) => {
                self.page = None;
                if let Some(prompt) = prompt.cloned() {
                    self.composer.update(cx, |c, cx| c.place_linked_prompt(prompt, vec![], window, cx));
                }
            }
            Err(message) => self.fail("Could not open the linked prompt", message.into(), cx),
        }
    }
    fn open_draft(&mut self, project: &str, link: NewThreadLink, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.start_draft(project, cx));
        self.page = None;
        self.fallback = None;
        let mut notes = vec![];
        if let NewThreadLink::New {
            provider: Some(provider),
            model,
            ..
        } = &link
        {
            let resolved = self
                .state
                .read(cx)
                .server_config
                .config
                .as_ref()
                .and_then(|config| rules::resolve_prompt_link_model_selection(&config.providers, provider, model.as_deref()));
            match resolved {
                Some(resolved) => {
                    if !resolved.exact {
                        notes.push(format!(
                            "The link asked for {}, which {provider} does not offer here, so its default model is used.",
                            model.as_deref().unwrap_or_default()
                        ));
                    }
                    self.composer.update(cx, |c, cx| c.linked_model(resolved.selection, window, cx));
                }
                None => notes.push(format!(
                    "The link asked for {provider}, which is not set up here, so your default model is used."
                )),
            }
        }
        let mut prompt = link.prompt().cloned();
        if let NewThreadLink::PullRequest { pull_request, .. } = &link {
            let p = prompt.get_or_insert(rules::LinkedPrompt {
                text: String::new(),
                hidden_characters: 0,
            });
            if !p.text.is_empty() {
                p.text.push_str("\n\n");
            }
            p.text.push_str(&pull_request.url());
        }
        if let Some(prompt) = prompt {
            self.composer.update(cx, |c, cx| c.place_linked_prompt(prompt, notes, window, cx));
        }
        cx.notify();
    }
    fn open_scratch(&mut self, link: NewThreadLink, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        if self.state.read(cx).scratch_root().is_none() {
            return self.fail(
                "Could not open the link",
                "None of your machines can start a thread without a project right now.".into(),
                cx,
            );
        }
        self.scratch = true;
        cx.notify();
        self.page = Some(("Opening the link...".into(), String::new()));
        let rpc = self.state.read(cx).rpc(t3_core::home::PROJECTS_ENSURE_SCRATCH, serde_json::json!({}));
        let sequence = self.sequence;
        let connection = self.state.read(cx).connection_id();
        cx.spawn_in(_window, async move |this, cx| {
            let result = rpc.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.sequence != sequence || this.state.read(cx).connection_id() != connection {
                    return;
                }
                this.scratch = false;
                match result.as_ref().ok().and_then(t3_core::home::ensure_scratch_project_id) {
                    Some(project) => this.open_draft(&project, link, window, cx),
                    None => this.fail("Could not open the link", "T3 Code could not open a new thread.".into(), cx),
                }
            });
        })
        .detach();
    }
}
impl Render for PromptLinks {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((title, description)) = self.page.clone() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        div()
            .absolute()
            .top(px(52.))
            .left(px(crate::workspace::sidebar_inline_width(self.state.read(cx), f32::from(window.viewport_size().width))))
            .right_0()
            .bottom_0()
            .occlude()
            .bg(hsla(p.background))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .max_w(px(448.))
                    .gap(px(12.))
                    .items_center()
                    .text_color(hsla(p.foreground))
                    .child(div().text_xl().child(title))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(description))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .when_some(self.pr_checkout.as_ref(), |row, checkout| {
                                let label = if checkout.checking_out { "Checking out..." } else { "Check out" };
                                row.child(
                                    Button::new("link-checkout-pr", ButtonVariant::Default, ButtonSize::Sm)
                                        .label(label)
                                        .disabled(checkout.checking_out)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_pr_checkout(window, cx);
                                        })),
                                )
                            })
                            .when(self.missing_repository.is_some() && self.fallback.is_some(), |row| {
                                row.child(
                                    Button::new("link-clone-repository", ButtonVariant::Default, ButtonSize::Sm)
                                        .label("Clone repository")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if let (Some(repo), Some(link)) = (this.missing_repository.take(), this.fallback.take()) {
                                                this.clone_missing_repository(repo, link, window, cx);
                                            }
                                        })),
                                )
                            })
                            .when(self.fallback.is_some(), |row| {
                                row.child(
                                    Button::new("link-without-project", ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Use without project")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.missing_repository = None;
                                            if let Some(link) = this.fallback.take() {
                                                this.open_scratch(link, window, cx);
                                            }
                                        })),
                                )
                            })
                            .child(
                                Button::new("dismiss-prompt-link", ButtonVariant::Ghost, ButtonSize::Sm)
                                    .label("Dismiss")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sequence += 1;
                                        this.page = None;
                                        this.pending = None;
                                        this.pending_target = None;
                                        this.missing_repository = None;
                                        this.pr_checkout = None;
                                        this.fallback = None;
                                        this.state.update(cx, |s, cx| s.close_toast_by_key(PR_LINK_CHECKOUT_TOAST_KEY, cx));
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::size;
    use serde_json::json;

    #[gpui::test]
    async fn unlinked_pr_link_confirms_and_runs_worktree_checkout_handoff(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });

        // Seed active environment with a matching GitHub repository project and a ready provider.
        state.update(cx, |s, cx| {
            let mut snapshot = s.shell.snapshot.clone().unwrap_or_default();
            snapshot.projects.push(serde_json::from_value(json!({
                "id": "proj-t3",
                "title": "t3code",
                "workspaceRoot": "/repo/t3code",
                "repositoryIdentity": { "canonicalKey": "github.com/pingdotgg/t3code" },
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-02T00:00:00Z"
            })).unwrap());
            s.shell.snapshot = Some(snapshot);
            s.shell.synchronized = true;
            s.server_config.config = Some(
                serde_json::from_value(json!({
                    "environment": { "environmentId": "fixture-env", "label": "Fixture", "capabilities": {} },
                    "providers": [{
                        "instanceId": "codex",
                        "driver": "codex",
                        "displayName": "Codex",
                        "enabled": true,
                        "installed": true,
                        "status": "ready",
                        "models": [{ "slug": "gpt-5.4", "name": "GPT-5.4", "isDefault": true }]
                    }],
                    "settings": {},
                    "issues": [],
                    "keybindings": []
                }))
                .unwrap(),
            );
            cx.emit(AppEvent::Shell);
        });

        let (prompt_links, vcx) = cx.add_window_view(|window, cx| {
            let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
            PromptLinks::new(state.clone(), composer, window, cx)
        });
        vcx.update(|window, cx| {
            window.resize(size(px(900.), px(640.)));
            window.draw(cx).clear();
        });

        // 1. Open an unlinked PR prompt link; server reports no existing linked threads.
        vcx.update(|_, cx| {
            state.update(cx, |s, cx| {
                s.open_prompt_url("https://app.t3.codes/new?pr=pingdotgg/t3code%23111&prompt=Review+PR".into(), cx);
            });
        });
        vcx.run_until_parked();
        let (method, payload, reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == t3_core::pull_requests::link::LINKED_THREADS {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(method, t3_core::pull_requests::link::LINKED_THREADS);
        assert_eq!(payload["repository"], "pingdotgg/t3code");
        assert_eq!(payload["number"], 111);
        let (confirm_tx, confirm_rx) = futures::channel::oneshot::channel::<()>();
        let confirm_tx = std::cell::RefCell::new(Some(confirm_tx));
        let _confirm_sub = vcx.update(|_, cx| {
            cx.observe(&prompt_links, move |view, cx| {
                if view.read(cx).pr_checkout.is_some()
                    && let Some(tx) = confirm_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply.send(Ok(json!({ "threads": [] }))).unwrap();
        confirm_rx.await.unwrap();
        vcx.run_until_parked();

        // Confirmation page must be shown without auto-running checkout.
        vcx.read_entity(&prompt_links, |view, _| {
            assert_eq!(
                view.page.as_ref().map(|(t, _)| t.as_str()),
                Some("Check out pingdotgg/t3code#111?")
            );
            assert!(view.pr_checkout.is_some());
        });

        // 2. User clicks Check out -> sends git.preparePullRequestThread (worktree + threadId),
        // then dispatches thread.create + thread.pull-request.link.
        vcx.update(|window, cx| {
            prompt_links.update(cx, |view, cx| view.start_pr_checkout(window, cx));
        });
        vcx.run_until_parked();

        let (prep_method, prep_payload, prep_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == t3_core::vcs::method::GIT_PREPARE_PULL_REQUEST_THREAD {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(prep_method, t3_core::vcs::method::GIT_PREPARE_PULL_REQUEST_THREAD);
        assert_eq!(prep_payload["cwd"], "/repo/t3code");
        assert_eq!(prep_payload["mode"], "worktree");
        assert_eq!(prep_payload["reference"], "https://github.com/pingdotgg/t3code/pull/111");
        let generated_thread_id = prep_payload["threadId"].as_str().expect("threadId sent").to_owned();
        let (done_tx, done_rx) = futures::channel::oneshot::channel::<()>();
        let done_tx = std::cell::RefCell::new(Some(done_tx));
        let expected_thread_id = generated_thread_id.clone();
        let _done_sub = vcx.update(|_, cx| {
            cx.subscribe(&state, move |s, _event: &AppEvent, cx| {
                if s.read(cx).selected_thread.as_deref() == Some(expected_thread_id.as_str())
                    && s.read(cx)
                        .toasts
                        .by_key(PR_LINK_CHECKOUT_TOAST_KEY)
                        .is_some_and(|t| t.title == "Checkout ready")
                    && let Some(tx) = done_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        prep_reply
            .send(Ok(json!({
                "pullRequest": {
                    "number": 111,
                    "title": "Close prompt links parity",
                    "url": "https://github.com/pingdotgg/t3code/pull/111",
                    "baseBranch": "main",
                    "headBranch": "feat/pr-111",
                    "state": "open"
                },
                "branch": "feat/pr-111",
                "worktreePath": "/repo/.worktrees/pr-111",
                "isOnPullRequestHead": true
            })))
            .unwrap();

        let mut saw_create = false;
        let mut saw_link = false;
        for _ in 0..4 {
            if saw_create && saw_link {
                break;
            }
            let (m, p, r) = fixture.next_request().await;
            if m == t3_core::commands::method::DISPATCH_COMMAND {
                match p["type"].as_str() {
                    Some("thread.create") => {
                        saw_create = true;
                        assert_eq!(p["threadId"], generated_thread_id);
                        assert_eq!(p["projectId"], "proj-t3");
                        assert_eq!(p["branch"], "feat/pr-111");
                        assert_eq!(p["worktreePath"], "/repo/.worktrees/pr-111");
                    }
                    Some("thread.pull-request.link") => {
                        saw_link = true;
                        assert_eq!(p["threadId"], generated_thread_id);
                        assert_eq!(p["number"], 111);
                        assert_eq!(p["repository"], "pingdotgg/t3code");
                    }
                    _ => {}
                }
            }
            let _ = r.send(Ok(json!({ "sequence": 1 })));
        }
        assert!(saw_create && saw_link, "expected thread.create and thread.pull-request.link");
        done_rx.await.unwrap();

        vcx.read_entity(&prompt_links, |view, _| {
            assert!(view.page.is_none(), "overlay closes after checkout completes");
            assert!(view.pr_checkout.is_none());
        });
        vcx.read_entity(&state, |s, _| {
            assert_eq!(s.selected_thread.as_deref(), Some(generated_thread_id.as_str()));
            let checkout_toast = s
                .toasts
                .by_key(PR_LINK_CHECKOUT_TOAST_KEY)
                .expect("checkout ready toast");
            assert_eq!(checkout_toast.kind, ToastKind::Success);
            assert_eq!(checkout_toast.title, "Checkout ready");
        });

        drop(_confirm_sub);
        drop(_done_sub);
        cx.update(|cx| {
            state.update(cx, |s, _| s.stop_pump());
        });
        drop(fixture);
        runtime.shutdown_background();
    }
}
