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
        self.prompt_link_urls.push_back(url);
        cx.emit(AppEvent::PromptLink);
    }
}

pub struct PromptLinks {
    state: Entity<AppState>,
    composer: Entity<Composer>,
    pending: Option<String>,
    page: Option<(String, String)>,
    fallback: Option<NewThreadLink>,
    scratch: bool,
    sequence: u64,
    connecting: Option<String>,
    _subscription: Subscription,
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
            page: None,
            fallback: None,
            scratch: false,
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
            self.scratch = false;
            self.pending = Some(url);
            self.page = Some(("Opening the link...".into(), String::new()));
            self.fallback = None;
            cx.notify();
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
        if let NewThreadLink::PullRequest { pull_request, .. } = &link
            && let Some(thread) = rules::find_prompt_link_pull_request_thread(&snapshot.threads, pull_request)
        {
            let id = thread.id.clone();
            let environment = app.active_environment().and_then(|e| e.environment_id.clone()).unwrap_or_default();
            self.open_thread(&environment, &id, link.prompt(), window, cx);
            cx.notify();
            return;
        }
        if let Some(repo) = link.repository() {
            let candidates = rules::find_prompt_link_projects(
                snapshot.projects.iter().filter(|p| p.archived_at.is_none() && !app.is_scratch_project(&p.id)),
                repo,
            );
            let owned: Vec<_> = candidates.into_iter().cloned().collect();
            let project = t3_core::home::most_recent_project(&owned, &snapshot.threads).map(|p| p.id.clone());
            if let Some(project) = project {
                if matches!(link, NewThreadLink::PullRequest { .. }) {
                    self.fail(
                        "Could not open the pull request",
                        "This pull request has no locally linked thread. Pull request checkout from a link is not available yet.".into(),
                        cx,
                    );
                    self.fallback = Some(link);
                } else {
                    self.open_draft(&project, link, window, cx);
                }
            } else {
                self.fail(
                    &format!("You don't have {}/{}", repo.owner, repo.name),
                    "Clone it to work on it here, or start without a project.".into(),
                    cx,
                );
                self.fallback = Some(link);
            }
        } else {
            self.open_scratch(link, window, cx);
        }
        cx.notify();
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((title, description)) = self.page.clone() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        div()
            .absolute()
            .top(px(52.))
            .left(px(if self.state.read(cx).sidebar.collapsed {
                0.
            } else {
                crate::workspace::SIDEBAR_WIDTH
            }))
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
                            .when(self.fallback.is_some(), |row| {
                                row.child(
                                    Button::new("link-without-project", ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Use without project")
                                        .on_click(cx.listener(|this, _, window, cx| {
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
                                        this.fallback = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
