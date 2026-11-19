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
    scratch: bool,
    cloning: bool,
    sequence: u64,
    connecting: Option<String>,
    _subscription: Subscription,
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

            let mut lookup_targets: Vec<(String, String, String)> = Vec::new();
            for proj in &active_owned {
                let env_id = app.active_environment().and_then(|e| e.environment_id.clone()).unwrap_or_else(|| active_saved.clone());
                lookup_targets.push((active_saved.clone(), env_id, proj.id.clone()));
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
                .map(|p| p.id.clone())
                .collect();
                let env_id = env
                    .config
                    .config
                    .as_ref()
                    .map(|c| c.environment.environment_id.clone())
                    .unwrap_or_else(|| saved_id.clone());
                for project_id in &matched {
                    lookup_targets.push((saved_id.clone(), env_id.clone(), project_id.clone()));
                }
                if fleet_best.is_none()
                    && let Some(first) = matched.into_iter().next()
                {
                    fleet_best = Some((saved_id.clone(), first));
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
        targets: Vec<(String, String, String)>,
        pull_request: rules::PullRequest,
        link: NewThreadLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sequence = self.sequence;
        let active_saved = self.state.read(cx).environments.active.clone().unwrap_or_default();
        let active_client = self.state.read(cx).client_handle();
        let client_futures: Vec<_> = {
            let app = self.state.read(cx);
            targets
                .into_iter()
                .map(|(saved_id, env_id, project_id)| {
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
                } else {
                    this.fail(
                        "Could not open the pull request",
                        "This pull request has no linked thread yet. Pull request checkout from a link is not available yet.".into(),
                        cx,
                    );
                    this.fallback = Some(link);
                }
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
                        this.open_draft(&started.project_id, link, window, cx);
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
                                        this.fallback = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
