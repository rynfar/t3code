//! Draft workspace menu and previous entry from `BranchToolbar.tsx`,
//! `BranchToolbarEnvModeSelector.tsx` and `PreviousWorktreeItemContent.tsx`.

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::commands::method;
use t3_core::server_config::ThreadEnvMode;
use t3_core::workspace::{self as rules, DraftWorkspace, PreviousThread, PreviousWorktreeSeed};

use super::controls::{ComposerControl, Float, MenuItem, floating, menu_surface};
use super::{Composer, Target, drafts};
use super::menu::MenuKey;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

#[derive(Clone, PartialEq, Eq)]
struct Scope {
    environment: String,
    project: String,
    root: String,
    connection: u64,
    generation: u64,
    destination: (String, String),
}

#[derive(Default)]
pub(super) struct Workspace {
    scope: Option<Scope>,
    is_repo: bool,
    default_mode: Option<ThreadEnvMode>,
    previous: Option<PreviousWorktreeSeed>,
    pub(super) open: bool,
    highlighted: usize,
    loading: bool,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl Composer {
    fn workspace_choice(&self) -> Option<&DraftWorkspace> {
        let scope = self.workspace.scope.as_ref()?;
        self.drafts.store.get(&drafts::key(&Target::Draft(scope.project.clone())))?.workspace.as_ref().filter(|choice| choice.in_scope(&scope.environment, &scope.root) && choice.destination.as_ref().is_none_or(|destination| destination == &scope.destination))
    }

    pub(super) fn selected_workspace(&self) -> Option<DraftWorkspace> {
        self.workspace_choice().cloned()
    }

    pub(super) fn sync_workspace(&mut self, reload: bool, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let scope = (|| {
            if self.fixed.is_some() {
                return None;
            }
            let Target::Draft(project) = self.snapshot.target.as_ref()? else {
                return None;
            };
            Some(Scope {
                environment: state.environments.active.clone()?,
                project: project.clone(),
                root: state.shell.project(project)?.workspace_root.clone(),
                connection: state.connection_id()?,
                generation: *state.client_handle()?.connection_generation().borrow(),
                destination: (state.server_config.config.as_ref()?.environment.environment_id.clone(), state.shell.project(project)?.created_at.clone()),
            })
        })();
        let changed = scope != self.workspace.scope;
        if changed {
            self.workspace = Workspace { scope: scope.clone(), ..Default::default() };
        }
        let Some(scope) = scope else { if changed {cx.notify();} return };
        let choice = self.workspace_choice();
        let current = choice.and_then(|choice| choice.previous.as_ref()).map(|seed| seed.worktree_path.as_str());
        let previous = rules::previous_worktree_seed(
            state.shell.snapshot.iter().flat_map(|shell| &shell.threads).map(|thread| PreviousThread {
                project_id: &thread.project_id,
                branch: thread.branch.as_deref(),
                worktree_path: thread.worktree_path.as_deref(),
                updated_at: &thread.updated_at,
                archived_at: thread.archived_at.as_deref(),
            }),
            &scope.project,
            current,
        );
        let previous_changed = previous != self.workspace.previous;
        self.workspace.previous = previous;
        if !changed && !reload {
            if previous_changed {cx.notify();}
            return;
        }
        let Some(client) = state.client_handle() else {
            cx.notify();
            return;
        };
        let Some(config) = state.server_config.config.as_ref() else {
            cx.notify();
            return;
        };
        let configured = rules::configured_env_mode(&config.settings, &scope.project);
        let new_root = config.new_projects_root.clone();
        let has_threads = state.shell.snapshot.as_ref().is_some_and(|shell| {
            shell.threads.iter().any(|thread| thread.project_id == scope.project)
                || shell.older_settled.as_ref().is_some_and(|cut| cut.counts.iter().any(|count| count.project_id == scope.project && count.count > 0))
        });
        let runtime = state.runtime().clone();
        cx.notify();
        let origin = scope.clone();
        self.workspace.loading = true;
        let net = runtime.spawn(async move {
            let file = if configured.is_none() {
                client
                    .request(method::PROJECTS_READ_FILE, rules::project_file_request(&origin.root))
                    .await
                    .ok()
                    .and_then(|value| rules::project_file_contents(&value).and_then(rules::project_file_env_mode))
            } else {
                None
            };
            let mode = rules::new_thread_env_mode(configured, file, Some(&origin.root), new_root.as_deref(), has_threads);
            let refs = client.list_refs(rules::list_refs_request(&origin.root)).await;
            (mode, refs)
        });
        self.workspace.task = Some(cx.spawn(async move |this, cx| {
            let Ok((mode, result)) = net.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.workspace.scope.as_ref() != Some(&scope) {
                    return;
                }
                this.workspace.loading = false;
                this.workspace.default_mode = Some(mode);
                match result {
                    Ok(Some(refs)) => {
                        this.workspace.is_repo = refs.is_repo;
                        this.workspace.error = None;
                    }
                    Ok(None) => {
                        this.sync_workspace(true, cx);
                    }
                    Err(error) => this.workspace.error = Some(error),
                }
                if !this.workspace_enabled() {this.workspace.open = false;}
                cx.notify();
            });
        }));
    }

    pub(crate) fn open_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workspace_enabled() {
            if self.workspace.open {self.workspace.open = false; cx.notify();}
            return;
        }
        self.workspace.open = !self.workspace.open;
        if self.workspace.open {
            let choice = self.workspace_choice();
            self.workspace.highlighted = usize::from(choice.map_or(self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local) == ThreadEnvMode::Worktree, |choice| choice.mode == ThreadEnvMode::Worktree && choice.previous.is_none()));
            self.sync_workspace(true, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    pub(super) fn workspace_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.workspace.open {return false;}
        self.sync_workspace(false, cx);
        if !self.workspace.open || !self.workspace_enabled() {self.workspace.open = false; cx.notify(); return false;}
        let previous = self.workspace.previous.is_some();
        let count = 2 + usize::from(previous) + usize::from(self.workspace.error.is_some());
        self.workspace.highlighted = self.workspace.highlighted.min(count - 1);
        match key {
            MenuKey::Up => self.workspace.highlighted = (self.workspace.highlighted + count - 1) % count,
            MenuKey::Down => self.workspace.highlighted = (self.workspace.highlighted + 1) % count,
            MenuKey::Dismiss => {self.workspace.open = false; self.focus(window, cx);}
            MenuKey::Accept => match self.workspace.highlighted {
                0 => { let previous = self.workspace_choice().and_then(|choice| choice.previous.clone()); self.choose_workspace(ThreadEnvMode::Local, previous, window, cx); }
                1 => self.choose_workspace(ThreadEnvMode::Worktree, None, window, cx),
                2 if previous => self.use_previous_worktree(window, cx),
                _ => self.sync_workspace(true, cx),
            },
        }
        cx.notify();
        true
    }

    fn workspace_visible(&self) -> bool {
        self.workspace.scope.as_ref().is_some_and(|scope| {
            self.workspace.is_repo || self.workspace.error.is_some()
                || (self.workspace_choice().is_none() && self.drafts.store.get(&drafts::key(&Target::Draft(scope.project.clone()))).is_some_and(|draft| draft.workspace.is_some()))
        })
    }

    pub(super) fn workspace_enabled(&self) -> bool {
        rules::can_use_previous_worktree(!matches!(self.snapshot.target, Some(Target::Draft(_))), self.sending, false) && self.workspace_visible()
    }

    fn choose_workspace(&mut self, mode: ThreadEnvMode, previous: Option<PreviousWorktreeSeed>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workspace_enabled() {
            return;
        }
        let Some(scope) = self.workspace.scope.clone() else {
            return;
        };
        let target = Target::Draft(scope.project);
        self.record_draft(&target, cx);
        let mut draft = self.drafts.store.get(&drafts::key(&target)).cloned().unwrap_or_default();
        draft.updated_at = crate::sidebar::controller::now_ms();
        draft.workspace = Some(DraftWorkspace { destination: Some(scope.destination), branch: None, environment_id: scope.environment, project_root: scope.root, mode, previous });
        self.drafts.store.set(&drafts::key(&target), draft);
        self.schedule_draft_save(&target, cx);
        self.workspace.open = false;
        self.error = None;
        self.sync_workspace(false, cx);
        self.sync_menu(true, cx);
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn use_previous_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_workspace(false, cx);
        if !self.workspace.is_repo {
            return;
        }
        if let Some(seed) = self.workspace.previous.clone() {
            self.choose_workspace(ThreadEnvMode::Worktree, Some(seed), window, cx);
        }
    }

    pub(super) fn render_workspace(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.workspace_visible() {
            return None;
        }
        let p = palette(cx);
        let choice = self.workspace_choice();
        let previous = choice.and_then(|choice| choice.previous.as_ref());
        let mode = choice.map_or(self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local), |choice| choice.mode);
        let label = if previous.is_some() {
            "Current worktree"
        } else if mode == ThreadEnvMode::Worktree {
            "New worktree"
        } else {
            "Current checkout"
        };
        let tooltip = previous.map(|seed| seed.branch.as_deref().unwrap_or(&seed.worktree_path)).unwrap_or(label);
        let tooltip = crate::keymap::label("composer.workspace", cx).map_or(tooltip.to_owned(), |shortcut| format!("{tooltip} · {shortcut}"));
        let enabled = self.workspace_enabled();
        let mut popup = menu_surface(cx)
            .w(px(336.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.workspace.open = false;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Workspace"))
            .child(
                MenuItem::new("workspace-local", if previous.is_some() { "Current worktree" } else { "Current checkout" })
                    .checked(mode == ThreadEnvMode::Local || previous.is_some())
                    .highlighted(self.workspace.highlighted == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let previous = this.workspace_choice().and_then(|choice| choice.previous.clone());
                        this.choose_workspace(ThreadEnvMode::Local, previous, window, cx);
                    })),
            )
            .child(
                MenuItem::new("workspace-new", "New worktree")
                    .checked(mode == ThreadEnvMode::Worktree && previous.is_none())
                    .highlighted(self.workspace.highlighted == 1)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_workspace(ThreadEnvMode::Worktree, None, window, cx))),
            );
        if let Some(seed) = &self.workspace.previous {
            popup = popup.child(
                div()
                    .id("workspace-previous")
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(self.workspace.highlighted == 2, |this| this.bg(hsla(p.accent)))
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(cx.listener(|this, _, window, cx| this.use_previous_worktree(window, cx)))
                    .child(
                        h_flex().items_start().gap(px(6.)).child(div().pt(px(4.)).child(Icon::History.render(px(12.), hsla(p.muted_foreground)))).child(
                            v_flex()
                                .min_w_0()
                                .text_sm()
                                .child("Previous worktree")
                                .children(seed.branch.clone().map(|branch| div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(branch))),
                        ),
                    ),
            );
        }
        if self.workspace.loading {
            popup = popup.child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Loading refs..."));
        }
        if let Some(error) = &self.workspace.error {
            popup = popup
                .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.error_foreground)).child(error.clone()))
                .child(MenuItem::new("workspace-retry", "Retry").highlighted(self.workspace.highlighted == 2 + usize::from(self.workspace.previous.is_some())).on_click(cx.listener(|this, _, _, cx| this.sync_workspace(true, cx))));
        }
        Some(
            div()
                .relative()
                .min_w_0()
                .child(
                    ComposerControl::new("composer-workspace")
                        .icon(if previous.is_some() {
                            Icon::FolderGit
                        } else if mode == ThreadEnvMode::Worktree {
                            Icon::FolderGit2
                        } else {
                            Icon::Folder
                        })
                        .label(label)
                        .when_some(previous.and_then(|seed| seed.branch.clone()), |control, branch| {
                            control.suffix(div().min_w_0().max_w(px(224.)).truncate().text_xs().text_color(hsla(p.muted_foreground)).child(branch))
                        })
                        .pressed(self.workspace.open)
                        .disabled(!enabled)
                        .tooltip(tooltip)
                        .on_click(cx.listener(|this, _, window, cx| this.open_workspace(window, cx))),
                )
                .when(self.workspace.open && enabled, |this| this.child(floating(Float::AboveStart, div().mb(px(4.)).child(popup))))
                .into_any_element(),
        )
    }
}

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn verify_workspace_availability(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.workspace.scope.is_none() {return Err("availability fixture requires a connected draft".into());}
        self.workspace.task = None;
        self.workspace.previous = None;
        self.workspace.is_repo = false;
        self.workspace.error = None;
        self.workspace.open = false;
        self.set_prompt("", window, cx);
        self.open_workspace(window, cx);
        if self.workspace.open || self.render_workspace(cx).is_some() {return Err("non-repository shortcut opened an invisible workspace popup".into());}
        if self.workspace_key(MenuKey::Down, window, cx) || self.workspace_key(MenuKey::Accept, window, cx) {return Err("hidden workspace popup captured prompt keys".into());}
        self.on_enter(false, window, cx);
        if cx.global::<super::SnapshotAudit>().submissions.replace(0) != 1 {return Err("hidden workspace popup suppressed ordinary empty Enter".into());}
        self.workspace.error = Some("fixture read failed".into());
        self.open_workspace(window, cx);
        if !self.workspace.open || self.render_workspace(cx).is_none() || !self.workspace_key(MenuKey::Down, window, cx) {return Err("visible error/retry popup lost keyboard access while refreshing".into());}
        self.workspace_key(MenuKey::Dismiss, window, cx);
        self.workspace.is_repo = true;
        self.workspace.error = None;
        self.open_workspace(window, cx);
        if !self.workspace.open {return Err("repository popup did not open".into());}
        self.workspace.is_repo = false;
        if self.workspace_key(MenuKey::Accept, window, cx) || self.workspace.open {return Err("workspace popup retained keys after repository availability changed".into());}
        self.workspace.open = true;
        self.on_enter(false, window, cx);
        if cx.global::<super::SnapshotAudit>().submissions.replace(0) != 1 {return Err("stale hidden popup suppressed empty Enter fallback".into());}
        self.workspace.open = false;
        self.workspace.task = None;
        eprintln!("workspace availability: hidden non-repo shortcut/keys refused; empty Enter reached submit spy; error/retry and repository menus remained accessible; loss of availability released keys; no launch RPC");
        Ok(())
    }
}

impl Composer {
    pub(super) fn apply_branch_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.state.update(cx, |state, _| state.branch_draft.take());
        let Some((owner, choice)) = pending else { return };
        let state = self.state.read(cx);
        if !owner.current(state) || state.selected_thread.is_some() { return; }
        let Some(project) = state.draft_project.clone() else { return };
        let target = Target::Draft(project);
        if self.snapshot.target.as_ref() != Some(&target) { return; }
        self.record_draft(&target, cx);
        let mut draft = self.drafts.store.get(&drafts::key(&target)).cloned().unwrap_or_default();
        draft.workspace = Some(choice);
        self.drafts.store.set(&drafts::key(&target), draft);
        self.schedule_draft_save(&target, cx);
        self.sync_workspace(false, cx);
        self.sync_menu(true, cx);
        self.focus(window, cx);
        cx.notify();
    }
}
