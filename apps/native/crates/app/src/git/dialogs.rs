//! The Git dialogs (`GitActionsControl.tsx`, `PullRequestThreadDialog.tsx`): "Commit changes"
//! (with selectable & clickable changed files and an optional message), the default-branch
//! confirmation, the 3-step `PublishRepositoryDialog`, and the `PullRequestThreadDialog` for
//! checking out a PR/MR into local or a dedicated worktree.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, BoxShadow, ClickEvent, Context, ElementId, Entity, FocusHandle, FontWeight, IntoElement, MouseButton, ParentElement, Render,
    RenderOnce, SharedString, Styled, Subscription, Task, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::git_actions::{
    self, CloneProtocol, PublishProviderDiscovery, PublishRepositoryResult, PublishVisibility, StackedAction, includes_commit,
    offered_publish_providers, publish_provider_readiness, requires_default_branch_confirmation, resolve_publish_provider, terminology,
    valid_publish_repository,
};
use t3_core::vcs::change_request_reference::parse_pull_request_reference;
use t3_core::vcs::{PullRequestCheckoutMode, ResolvedPullRequest, format_diff_count};

use super::{GitEvent, GitStore};
use crate::composer::controls::checkbox;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant, focusable, tooltip_view};
use crate::theme::{hsla, palette};

struct PendingDefault {
    action: StackedAction,
    branch: String,
    includes_commit: bool,
    commit_message: Option<String>,
    file_paths: Option<Vec<String>>,
}

#[derive(Default)]
struct CommitDraft {
    excluded: HashSet<String>,
    editing_files: bool,
}

struct PublishDraft {
    step: u8,
    selected_provider: Option<String>,
    repo_override: bool,
    visibility: PublishVisibility,
    protocol: CloneProtocol,
    advanced_open: bool,
    discovery: Vec<PublishProviderDiscovery>,
    discovery_loading: bool,
    pending: bool,
    error: Option<String>,
    result: Option<PublishRepositoryResult>,
    generation: u64,
    _discovery_task: Option<Task<()>>,
    _publish_task: Option<Task<()>>,
}

impl Default for PublishDraft {
    fn default() -> Self {
        PublishDraft {
            step: 0,
            selected_provider: None,
            repo_override: false,
            visibility: PublishVisibility::Private,
            protocol: CloneProtocol::Ssh,
            advanced_open: false,
            discovery: Vec::new(),
            discovery_loading: false,
            pending: false,
            error: None,
            result: None,
            generation: 0,
            _discovery_task: None,
            _publish_task: None,
        }
    }
}

#[derive(Default)]
struct CheckoutPrDraft {
    dirty: bool,
    resolved: Option<ResolvedPullRequest>,
    resolve_error: Option<String>,
    resolving: bool,
    preparing: Option<PullRequestCheckoutMode>,
    prepare_error: Option<String>,
    generation: u64,
    _resolve_task: Option<Task<()>>,
    _prepare_task: Option<Task<()>>,
}

pub struct GitDialogs {
    git: Entity<GitStore>,
    commit: Option<CommitDraft>,
    pending: Option<PendingDefault>,
    publish: Option<PublishDraft>,
    checkout_pr: Option<CheckoutPrDraft>,
    message: Entity<TextareaState>,
    publish_repo: Entity<InputState>,
    publish_remote: Entity<InputState>,
    pr_input: Entity<InputState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl GitDialogs {
    pub fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| TextareaState::new(window, cx).placeholder("Leave empty to auto-generate").auto_grow(3, 8));
        let publish_repo = cx.new(|cx| InputState::new(window, cx).placeholder("owner/repo"));
        let publish_remote = cx.new(|cx| InputState::new(window, cx).placeholder("origin"));
        let pr_input = cx.new(|cx| InputState::new(window, cx).placeholder("PR URL, checkout command, or #42"));
        let subscriptions = vec![
            cx.subscribe(&git, |this, _, _: &GitEvent, cx| {
                if this.commit.is_some() || this.pending.is_some() || this.publish.is_some() || this.checkout_pr.is_some() {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&publish_repo, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(draft) = this.publish.as_mut() {
                        draft.repo_override = true;
                        draft.error = None;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. }
                    if this.publish.as_ref().is_some_and(|draft| draft.step == 1) =>
                {
                    this.submit_publish(window, cx);
                }
                _ => {}
            }),
            cx.subscribe_in(&pr_input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    this.on_pr_reference_changed(&value, cx);
                }
                InputEvent::PressEnter { .. } if this.checkout_pr.is_some() => {
                    this.confirm_checkout_pr(PullRequestCheckoutMode::Worktree, window, cx);
                }
                _ => {}
            }),
        ];
        GitDialogs {
            git,
            commit: None,
            pending: None,
            publish: None,
            checkout_pr: None,
            message,
            publish_repo,
            publish_remote,
            pr_input,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// `runGitActionWithToast`: pushing from the default branch asks first; anything else runs.
    pub fn request_action(&mut self, request: ActionRequest, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.git.read(cx);
        if !store.can_write_source_control() || (request.feature_branch && !store.can_change_target_branch()) {
            return;
        }
        let ActionRequest { action, commit_message, file_paths, feature_branch, skip_prompt } = request;
        let status = store.status.clone();
        let branch = status.as_ref().and_then(|status| status.local.ref_name.clone());
        let is_default = !feature_branch && status.as_ref().is_some_and(|status| status.local.is_default_ref);
        let has_changes = status.as_ref().is_some_and(|status| status.local.has_working_tree_changes);
        if !skip_prompt
            && requires_default_branch_confirmation(action, is_default)
            && let Some(branch) = branch
        {
            self.pending = Some(PendingDefault { action, branch, includes_commit: includes_commit(action, has_changes, feature_branch), commit_message, file_paths });
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.git.update(cx, |git, cx| git.run_stacked_action(action, commit_message, file_paths, feature_branch, cx));
    }

    /// `continuePendingDefaultBranchAction`.
    fn continue_pending(&mut self, feature_branch: bool, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.git.read(cx);
        if !store.can_write_source_control() || (feature_branch && (!store.can_change_target_branch() || self.feature_branch_unavailable(cx).is_some())) {
            return;
        }
        let Some(pending) = self.pending.take() else { return };
        cx.notify();
        let request = ActionRequest { action: pending.action, commit_message: pending.commit_message, file_paths: pending.file_paths, feature_branch, skip_prompt: true };
        self.request_action(request, window, cx);
    }

    /// Kept enabled until live status loads; the server rejects a stale request.
    fn feature_branch_unavailable(&self, cx: &App) -> Option<String> {
        let pending = self.pending.as_ref()?;
        let has_changes = self.git.read(cx).status.as_ref().is_none_or(|status| status.local.has_working_tree_changes);
        git_actions::feature_branch_unavailable_reason(&pending.branch, has_changes)
    }

    pub fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restored = self.git.update(cx, |git, _| git.take_failed_commit_message()).unwrap_or_default();
        self.commit = Some(CommitDraft::default());
        self.message.update(cx, |input, cx| {
            input.set_value(restored, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.publish.as_ref().is_some_and(|d| d.pending) || self.checkout_pr.as_ref().is_some_and(|d| d.preparing.is_some()) {
            return;
        }
        self.commit = None;
        self.pending = None;
        self.publish = None;
        self.checkout_pr = None;
        self.message.update(cx, |input, cx| input.set_value("", window, cx));
        self.git.update(cx, |git, _| git.clear_failed_commit_message());
        cx.notify();
    }

    /// Opens `PublishRepositoryDialog` (`GitActionsControl.tsx`) and loads `sourceControl.discovery`.
    pub fn open_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit = None;
        self.pending = None;
        self.checkout_pr = None;
        self.publish_repo.update(cx, |input, cx| input.set_value("", window, cx));
        self.publish_remote.update(cx, |input, cx| input.set_value("origin", window, cx));
        let mut draft = PublishDraft { discovery_loading: true, generation: 1, ..Default::default() };
        if let Some(task) = self.git.read(cx).discover_publish_providers(cx) {
            let generation = draft.generation;
            draft._discovery_task = Some(cx.spawn_in(window, async move |this, cx| {
                let discovered = task.await.ok().and_then(Result::ok).unwrap_or_default();
                let _ = this.update_in(cx, |dialogs, window, cx| {
                    let Some(draft) = dialogs.publish.as_mut() else { return };
                    if draft.generation != generation {
                        return;
                    }
                    draft.discovery = discovered;
                    draft.discovery_loading = false;
                    dialogs.sync_publish_prefill(window, cx);
                    cx.notify();
                });
            }));
        } else {
            draft.discovery_loading = false;
        }
        self.publish = Some(draft);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn sync_publish_prefill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.publish.as_ref() else { return };
        if draft.repo_override {
            return;
        }
        let offered = self.git.read(cx).offered_source_control_providers(cx);
        let provider = resolve_publish_provider(draft.selected_provider.as_deref(), offered.as_ref(), &draft.discovery);
        let account = draft.discovery.iter().find(|d| d.kind == provider.kind).and_then(|d| d.auth_account.as_deref());
        let prefill = account.map(|acct| format!("{acct}/")).unwrap_or_default();
        self.publish_repo.update(cx, |input, cx| input.set_value(prefill, window, cx));
    }

    fn submit_publish(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let offered = self.git.read(cx).offered_source_control_providers(cx);
        let repo_value = self.publish_repo.read(cx).value().trim().to_owned();
        let remote_value = self.publish_remote.read(cx).value().trim().to_owned();
        let Some(draft) = self.publish.as_mut() else { return };
        if draft.pending || !self.git.read(cx).can_write_source_control() {
            return;
        }
        let provider = resolve_publish_provider(draft.selected_provider.as_deref(), offered.as_ref(), &draft.discovery);
        if !publish_provider_readiness(provider.kind, &draft.discovery).ready || !valid_publish_repository(&repo_value) {
            return;
        }
        let visibility = draft.visibility;
        let protocol = draft.protocol;
        let Some(task) = self.git.read(cx).publish_repository(provider.kind, &repo_value, visibility, &remote_value, protocol, cx) else {
            return;
        };
        draft.pending = true;
        draft.error = None;
        let generation = draft.generation;
        draft._publish_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("An error occurred.".into()));
            let _ = this.update(cx, |dialogs, cx| {
                let Some(draft) = dialogs.publish.as_mut() else { return };
                if draft.generation != generation {
                    return;
                }
                draft.pending = false;
                match result {
                    Ok(published) => {
                        draft.result = Some(published);
                        draft.step = 2;
                        dialogs.git.update(cx, |git, cx| git.refresh_status(false, cx));
                    }
                    Err(error) => draft.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Opens `PullRequestThreadDialog` (`GT-BRANCH`) with an optional initial PR URL / `#123`.
    pub fn open_checkout_pr(&mut self, initial_reference: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit = None;
        self.pending = None;
        self.publish = None;
        let initial = initial_reference.unwrap_or_default();
        self.pr_input.update(cx, |input, cx| {
            input.set_value(initial.clone(), window, cx);
            input.focus(window, cx);
        });
        self.checkout_pr = Some(CheckoutPrDraft::default());
        if !initial.trim().is_empty() {
            self.schedule_pr_resolve(&initial, false, cx);
        }
        cx.notify();
    }

    fn on_pr_reference_changed(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some(draft) = self.checkout_pr.as_mut() else { return };
        draft.dirty = true;
        draft.prepare_error = None;
        self.schedule_pr_resolve(value, true, cx);
    }

    fn schedule_pr_resolve(&mut self, raw: &str, debounce: bool, cx: &mut Context<Self>) {
        let Some(draft) = self.checkout_pr.as_mut() else { return };
        draft.generation += 1;
        let generation = draft.generation;
        draft.resolved = None;
        draft.resolve_error = None;
        let Some(parsed) = parse_pull_request_reference(raw) else {
            draft.resolving = false;
            draft._resolve_task = None;
            cx.notify();
            return;
        };
        draft.resolving = true;
        draft._resolve_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(Duration::from_millis(450)).await;
            }
            let task = this.update(cx, |dialogs, cx| {
                let draft = dialogs.checkout_pr.as_ref()?;
                if draft.generation != generation {
                    return None;
                }
                dialogs.git.read(cx).resolve_pull_request(&parsed, cx)
            });
            let Ok(Some(task)) = task else {
                let _ = this.update(cx, |dialogs, cx| {
                    if let Some(draft) = dialogs.checkout_pr.as_mut().filter(|d| d.generation == generation) {
                        draft.resolving = false;
                        cx.notify();
                    }
                });
                return;
            };
            let result = task.await.unwrap_or_else(|_| Err("Failed to resolve pull request.".into()));
            let _ = this.update(cx, |dialogs, cx| {
                let Some(draft) = dialogs.checkout_pr.as_mut().filter(|d| d.generation == generation) else { return };
                draft.resolving = false;
                match result {
                    Ok(resolved) => {
                        draft.resolved = Some(resolved.pull_request);
                        draft.resolve_error = None;
                    }
                    Err(error) => draft.resolve_error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn confirm_checkout_pr(&mut self, mode: PullRequestCheckoutMode, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.pr_input.read(cx).value().to_string();
        let parsed = parse_pull_request_reference(&raw);
        let Some(draft) = self.checkout_pr.as_mut() else { return };
        if draft.preparing.is_some() || draft.resolving {
            return;
        }
        let Some(reference) = parsed else {
            draft.dirty = true;
            cx.notify();
            return;
        };
        if draft.resolved.is_none() {
            return;
        }
        let git = self.git.read(cx);
        if !git.can_write_source_control() || (mode == PullRequestCheckoutMode::Worktree && !git.can_operate_thread()) {
            return;
        }
        let Some((origin, task)) = git.prepare_pull_request_thread(&reference, mode, cx) else { return };
        let terms = terminology(git.status.as_ref().and_then(|s| s.provider_kind()));
        draft.preparing = Some(mode);
        draft.prepare_error = None;
        let generation = draft.generation;
        draft._prepare_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err(format!("Failed to prepare {} thread.", terms.singular)));
            let _ = this.update(cx, |dialogs, cx| {
                let Some(draft) = dialogs.checkout_pr.as_mut().filter(|d| d.generation == generation) else { return };
                draft.preparing = None;
                match result {
                    Ok(prepared) => {
                        dialogs.checkout_pr = None;
                        dialogs.git.update(cx, |git, cx| git.apply_prepared_pull_request(&origin, &prepared, cx));
                    }
                    Err(error) => draft.prepare_error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// `runDialogAction` and `runDialogActionOnNewBranch`.
    fn commit(&mut self, feature_branch: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.commit.take() else { return };
        let message = self.message.read(cx).value().trim().to_owned();
        let files = self.git.read(cx).status.as_ref().map(|status| status.local.working_tree.files.clone()).unwrap_or_default();
        let selected: Vec<String> = files.iter().filter(|file| !draft.excluded.contains(&file.path)).map(|file| file.path.clone()).collect();
        let file_paths = (!draft.excluded.is_empty()).then_some(selected);
        cx.notify();
        let request = ActionRequest { action: StackedAction::Commit, commit_message: (!message.is_empty()).then_some(message), file_paths, feature_branch, skip_prompt: feature_branch };
        self.request_action(request, window, cx);
    }

    fn toggle_file(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(draft) = self.commit.as_mut() {
            if !draft.excluded.remove(path) {
                draft.excluded.insert(path.to_owned());
            }
            cx.notify();
        }
    }

    fn toggle_all(&mut self, all: Vec<String>, cx: &mut Context<Self>) {
        if let Some(draft) = self.commit.as_mut() {
            draft.excluded = if draft.excluded.is_empty() { all.into_iter().collect() } else { HashSet::new() };
            cx.notify();
        }
    }

    fn render_commit(&self, draft: &CommitDraft, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let status = self.git.read(cx).status.clone();
        let files = status.as_ref().map(|status| status.local.working_tree.files.clone()).unwrap_or_default();
        let branch = status.as_ref().and_then(|status| status.local.ref_name.clone()).unwrap_or_else(|| "(detached HEAD)".into());
        let is_default = status.as_ref().is_some_and(|status| status.local.is_default_ref);
        let all_selected = draft.excluded.is_empty();
        let selected: Vec<_> = files.iter().filter(|file| !draft.excluded.contains(&file.path)).collect();
        let none_selected = selected.is_empty();
        let (insertions, deletions) = selected.iter().fold((0, 0), |(i, d), file| (i + file.insertions, d + file.deletions));
        let mono = cx.theme().mono_font_family.clone();
        let editing = draft.editing_files;
        let all_paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
        let stat = |insertions: u64, deletions: u64| {
            h_flex()
                .flex_none()
                .child(div().text_color(hsla(p.diff_addition)).child(format!("+{}", format_diff_count(insertions))))
                .child(div().text_color(hsla(p.muted_foreground)).child(" / "))
                .child(div().text_color(hsla(p.diff_deletion)).child(format!("-{}", format_diff_count(deletions))))
        };
        let can_open_file = self.git.read(cx).can_operate_thread()
            && status.as_ref().and_then(|s| s.local.working_tree.root_path.as_ref()).is_some();
        let file_rows = files.iter().enumerate().map(|(index, file)| {
            let excluded = draft.excluded.contains(&file.path);
            let toggle_path = file.path.clone();
            let open_path = file.path.clone();
            let open_tip: SharedString = format!("Open {} in editor", file.path).into();
            h_flex()
                .id(ElementId::Name(format!("commit-file-{index}").into()))
                .w_full()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .hover(|style| style.bg(hsla(p.accent).opacity(0.5)))
                .when(editing, |this| {
                    this.child(
                        div()
                            .id(ElementId::Name(format!("commit-file-check-{index}").into()))
                            .cursor_pointer()
                            .child(checkbox(!excluded, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_file(&toggle_path, cx);
                            })),
                    )
                })
                .child(
                    h_flex()
                        .id(ElementId::Name(format!("commit-file-open-{index}").into()))
                        .flex_1()
                        .min_w_0()
                        .gap(px(12.))
                        .justify_between()
                        .when(can_open_file, |this| {
                            this.cursor_pointer()
                                .tooltip(move |window, cx| tooltip_view(open_tip.clone(), window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.git.update(cx, |git, cx| git.open_changed_file(&open_path, cx));
                                }))
                        })
                        .child(div().flex_1().min_w_0().truncate().when(excluded, |this| this.text_color(hsla(p.muted_foreground))).child(file.path.clone()))
                        .child(if excluded {
                            div().flex_none().text_color(hsla(p.muted_foreground)).child("Excluded").into_any_element()
                        } else {
                            stat(file.insertions, file.deletions).into_any_element()
                        }),
                )
        });
        let files_header = h_flex()
            .justify_between()
            .child(
                h_flex()
                    .gap(px(8.))
                    .when(editing && !files.is_empty(), |this| {
                        this.child(div().id("commit-files-all").cursor_pointer().child(checkbox(all_selected, cx)).on_click(cx.listener(move |this, _, _, cx| this.toggle_all(all_paths.clone(), cx))))
                    })
                    .child(div().text_color(hsla(p.muted_foreground)).child("Files"))
                    .when(!all_selected && !editing, |this| this.child(div().text_color(hsla(p.muted_foreground)).child(format!("({} of {})", selected.len(), files.len())))),
            )
            .when(!files.is_empty(), |this| {
                this.child(Button::new("commit-files-edit").variant(Variant::Ghost).size(Size::Xs).label(if editing { "Done" } else { "Edit" }).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(draft) = this.commit.as_mut() {
                        draft.editing_files = !draft.editing_files;
                        cx.notify();
                    }
                })))
            });
        let summary = v_flex()
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(12.))
            .bg(hsla(p.secondary))
            .border_1()
            .border_color(hsla(p.border).opacity(0.5))
            .text_sm()
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().text_color(hsla(p.muted_foreground)).child("Branch"))
                    .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(branch))
                    .when(is_default, |this| this.child(div().flex_none().text_color(hsla(p.warning_foreground)).child("Default branch"))),
            )
            .child(
                v_flex().gap(px(4.)).child(files_header).child(if files.is_empty() {
                    div().font_weight(FontWeight::MEDIUM).child("none").into_any_element()
                } else {
                    v_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("commit-files")
                                .h(px(176.))
                                .overflow_y_scroll()
                                .rounded(px(8.))
                                .bg(hsla(p.card))
                                .border_1()
                                .border_color(hsla(p.border).opacity(0.5))
                                .p(px(4.))
                                .font_family(mono.clone())
                                .children(file_rows),
                        )
                        .child(h_flex().justify_end().font_family(mono).child(stat(insertions, deletions)))
                        .into_any_element()
                }),
            );
        let body = v_flex()
            .gap(px(16.))
            .child(summary)
            .child(v_flex().gap(px(4.)).child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Commit message (optional)")).child(Textarea::new(&self.message)));
        let store = self.git.read(cx);
        let can_write = store.can_write_source_control();
        let can_change_branch = store.can_change_target_branch();
        let footer = h_flex()
            .justify_end()
            .gap(px(8.))
            .child(DialogButton::new("commit-cancel", "Cancel", false).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))))
            .child(DialogButton::new("commit-new-branch", "Commit on new branch", false).disabled(!can_change_branch || none_selected).on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))))
            .child(DialogButton::new("commit-confirm", "Commit", true).disabled(!can_write || none_selected).on_click(cx.listener(|this, _, window, cx| this.commit(false, window, cx))));
        let frame = Frame {
            id: "commit-dialog",
            max_width: 512.,
            title: git_actions::COMMIT_DIALOG_TITLE.into(),
            description: git_actions::COMMIT_DIALOG_DESCRIPTION.into(),
            note: None,
            body: Some(body.into_any_element()),
            footer: footer.into_any_element(),
        };
        frame.render(self.focus.clone(), cx)
    }

    fn render_pending(&self, pending: &PendingDefault, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let store = self.git.read(cx);
        let can_write = store.can_write_source_control();
        let can_change_branch = store.can_change_target_branch();
        let terms = terminology(store.status.as_ref().and_then(|status| status.provider_kind()));
        let copy = git_actions::default_branch_dialog_copy(pending.action, &pending.branch, pending.includes_commit, terms);
        let unavailable = self.feature_branch_unavailable(cx);
        let footer = h_flex()
            .flex_wrap()
            .gap(px(8.))
            .child(div().mr_auto().child(DialogButton::new("default-branch-abort", "Abort", false).on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))))
            .child(DialogButton::new("default-branch-continue", copy.continue_label, false).disabled(!can_write).on_click(cx.listener(|this, _, window, cx| this.continue_pending(false, window, cx))))
            .child(
                DialogButton::new("default-branch-feature", "Check out feature branch & continue", true)
                    .disabled(!can_change_branch || unavailable.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.continue_pending(true, window, cx))),
            );
        let frame = Frame {
            id: "default-branch-dialog",
            max_width: 576.,
            title: copy.title.into(),
            description: copy.description.into(),
            note: unavailable.map(SharedString::from),
            body: None,
            footer: footer.into_any_element(),
        };
        frame.render(self.focus.clone(), cx)
    }

    fn render_publish(&self, draft: &PublishDraft, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let offered = self.git.read(cx).offered_source_control_providers(cx);
        let options = offered_publish_providers(offered.as_ref(), &draft.discovery);
        let has_ready = options.iter().any(|(_, r)| r.ready);
        let provider = resolve_publish_provider(draft.selected_provider.as_deref(), offered.as_ref(), &draft.discovery);
        let selected_readiness = publish_provider_readiness(provider.kind, &draft.discovery);
        let signed_in_host = draft.discovery.iter().find(|d| d.kind == provider.kind).and_then(|d| d.auth_host.as_deref());
        let publish_host = provider.publish_host(signed_in_host);
        let repo_value = self.publish_repo.read(cx).value().to_string();
        let can_write = self.git.read(cx).can_write_source_control();
        let can_submit = can_write && has_ready && selected_readiness.ready && !draft.pending && valid_publish_repository(&repo_value);
        let step_labels = ["Provider", "Repository", "Summary"];
        let steps_bar = h_flex().gap(px(8.)).pb(px(12.)).children(step_labels.iter().enumerate().map(|(ix, label)| {
            let active = draft.step as usize == ix;
            let completed = (draft.step as usize) > ix;
            let can_jump = draft.step != 2 && ix < draft.step as usize && !draft.pending;
            let text_color = if active { hsla(p.foreground) } else { hsla(p.muted_foreground) };
            let badge_bg = if active || completed { hsla(p.primary) } else { hsla(p.muted) };
            let badge_fg = if active || completed { hsla(p.primary_foreground) } else { hsla(p.muted_foreground) };
            h_flex()
                .id(("publish-step", ix))
                .gap(px(6.))
                .items_center()
                .when(can_jump, |this| {
                    this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.publish.as_mut() {
                            d.step = ix as u8;
                            cx.notify();
                        }
                    }))
                })
                .child(div().size(px(18.)).rounded_full().bg(badge_bg).text_color(badge_fg).flex().items_center().justify_center().text_xs().child(format!("{}", ix + 1)))
                .child(div().text_xs().font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(text_color).child(*label))
        }));

        let step_body: gpui::AnyElement = match draft.step {
            0 => {
                let cards: Vec<_> = options
                    .iter()
                    .enumerate()
                    .map(|(ix, (option, readiness))| {
                        let kind = option.kind;
                        let is_selected = provider.kind == kind && readiness.ready;
                        if !readiness.ready {
                            let hint: SharedString = readiness
                                .hint
                                .clone()
                                .unwrap_or_else(|| "Open Settings -> Source control to configure this provider.".into())
                                .into();
                            return h_flex()
                                .id(("publish-provider", ix))
                                .w_full()
                                .px(px(12.))
                                .py(px(10.))
                                .gap(px(10.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.secondary).opacity(0.6))
                                .opacity(0.7)
                                .child(Icon::Globe.sized(px(16.)).text_color(hsla(p.muted_foreground)))
                                .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(option.label))
                                .child(
                                    Button::new(("publish-setup-required", ix))
                                        .variant(Variant::Secondary)
                                        .size(Size::Xs)
                                        .label("Setup Required")
                                        .tooltip(hint)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close(window, cx);
                                            crate::settings::open(t3_core::settings::search::Section::SourceControl, cx);
                                        })),
                                )
                                .into_any_element();
                        }
                        h_flex()
                            .id(("publish-provider", ix))
                            .w_full()
                            .px(px(12.))
                            .py(px(10.))
                            .gap(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if is_selected { hsla(p.primary) } else { hsla(p.border) })
                            .bg(if is_selected { hsla(p.primary).opacity(0.1) } else { hsla(p.secondary) })
                            .cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent).opacity(0.5)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(d) = this.publish.as_mut() {
                                    d.selected_provider = Some(kind.to_owned());
                                    d.repo_override = false;
                                }
                                this.sync_publish_prefill(window, cx);
                                cx.notify();
                            }))
                            .child(Icon::Globe.sized(px(16.)).text_color(if is_selected { hsla(p.primary) } else { hsla(p.muted_foreground) }))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(option.label))
                                    .child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(option.description)),
                            )
                            .when(is_selected, |this| this.child(Icon::Check.sized(px(14.)).text_color(hsla(p.primary))))
                            .into_any_element()
                    })
                    .collect();
                v_flex()
                    .gap(px(8.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Provider"))
                    .when(draft.discovery_loading && draft.discovery.is_empty(), |this| {
                        this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Checking source control providers..."))
                    })
                    .child(v_flex().gap(px(8.)).children(cards))
                    .into_any_element()
            }
            1 => {
                let mono = cx.theme().mono_font_family.clone();
                let vis_card = |id: &'static str, vis: PublishVisibility, label: &'static str, desc: &'static str, icon: Icon| {
                    let selected = draft.visibility == vis;
                    h_flex()
                        .id(id)
                        .flex_1()
                        .px(px(12.))
                        .py(px(10.))
                        .gap(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if selected { hsla(p.primary) } else { hsla(p.border) })
                        .bg(if selected { hsla(p.primary).opacity(0.1) } else { hsla(p.secondary) })
                        .when(!draft.pending, |this| {
                            this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(d) = this.publish.as_mut() {
                                    d.visibility = vis;
                                    cx.notify();
                                }
                            }))
                        })
                        .child(icon.sized(px(16.)).text_color(hsla(p.muted_foreground)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(desc)),
                        )
                };
                let proto_btn = |id: &'static str, proto: CloneProtocol, label: &'static str| {
                    let selected = draft.protocol == proto;
                    Button::new(id)
                        .variant(if selected { Variant::Secondary } else { Variant::Ghost })
                        .size(Size::Xs)
                        .label(label)
                        .disabled(draft.pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(d) = this.publish.as_mut() {
                                d.protocol = proto;
                                cx.notify();
                            }
                        }))
                };
                v_flex()
                    .gap(px(14.))
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Repository"))
                            .child(
                                h_flex()
                                    .w_full()
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(hsla(p.input))
                                    .bg(hsla(p.background))
                                    .overflow_hidden()
                                    .child(
                                        h_flex()
                                            .flex_none()
                                            .px(px(10.))
                                            .py(px(6.))
                                            .border_r_1()
                                            .border_color(hsla(p.input))
                                            .bg(hsla(p.muted).opacity(0.5))
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(format!("{publish_host}/")),
                                    )
                                    .child(div().flex_1().min_w_0().font_family(mono).child(Input::new(&self.publish_repo).appearance(false).bordered(false).disabled(draft.pending))),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Visibility"))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(vis_card("publish-vis-private", PublishVisibility::Private, "Private", "Only invited people", Icon::Lock))
                                    .child(vis_card("publish-vis-public", PublishVisibility::Public, "Public", "Anyone on the web", Icon::Globe)),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(8.))
                            .child(
                                h_flex()
                                    .id("publish-advanced-toggle")
                                    .gap(px(4.))
                                    .items_center()
                                    .cursor_pointer()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.muted_foreground))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(d) = this.publish.as_mut() {
                                            d.advanced_open = !d.advanced_open;
                                            cx.notify();
                                        }
                                    }))
                                    .child((if draft.advanced_open { Icon::ChevronDown } else { Icon::ChevronRight }).sized(px(14.)))
                                    .child("Advanced"),
                            )
                            .when(draft.advanced_open, |this| {
                                this.child(
                                    h_flex()
                                        .gap(px(12.))
                                        .items_end()
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .gap(px(4.))
                                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Remote"))
                                                .child(Input::new(&self.publish_remote).disabled(draft.pending)),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_none()
                                                .gap(px(4.))
                                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Protocol"))
                                                .child(h_flex().gap(px(4.)).child(proto_btn("publish-proto-ssh", CloneProtocol::Ssh, "SSH")).child(proto_btn("publish-proto-https", CloneProtocol::Https, "HTTPS"))),
                                        ),
                                )
                            }),
                    )
                    .when(draft.pending, |this| {
                        this.child(
                            h_flex()
                                .gap(px(8.))
                                .px(px(10.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(hsla(p.muted).opacity(0.4))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(Icon::LoaderCircle.sized(px(14.)))
                                .child(format!("Publishing repository to {}...", provider.label)),
                        )
                    })
                    .children(draft.error.as_ref().filter(|_| !draft.pending).map(|error| {
                        v_flex()
                            .gap(px(2.))
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(hsla(p.destructive).opacity(0.4))
                            .bg(hsla(p.destructive).opacity(0.1))
                            .text_xs()
                            .text_color(hsla(p.destructive_foreground))
                            .child(div().font_weight(FontWeight::MEDIUM).child("Publish failed"))
                            .child(error.clone())
                    }))
                    .into_any_element()
            }
            _ => {
                let mono = cx.theme().mono_font_family.clone();
                match &draft.result {
                    Some(result) => {
                        let url = result.repository.url.clone();
                        let heading = if result.status == "pushed" { "Repository published" } else { "Repository created" };
                        let subtitle = if result.status == "pushed" {
                            format!("{} is now live on {}.", result.branch, provider.label)
                        } else {
                            format!("Remote \"{}\" is set up. Make a commit and push it to share your code.", result.remote_name)
                        };
                        v_flex()
                            .gap(px(12.))
                            .items_center()
                            .child(
                                div()
                                    .size(px(32.))
                                    .rounded_full()
                                    .bg(hsla(p.success).opacity(0.15))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(Icon::Check.sized(px(16.)).text_color(hsla(p.success))),
                            )
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(heading))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(subtitle))
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap(px(8.))
                                    .px(px(12.))
                                    .py(px(8.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hsla(p.border))
                                    .bg(hsla(p.secondary))
                                    .child(Icon::Globe.sized(px(14.)).text_color(hsla(p.muted_foreground)))
                                    .child(div().flex_1().min_w_0().truncate().font_family(mono).text_xs().child(result.repository.name_with_owner.clone())),
                            )
                            .child(
                                DialogButton::new("publish-open-remote", format!("Open on {}", provider.label), false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.git.update(cx, |git, cx| git.open_external_url(&url, cx));
                                })),
                            )
                            .into_any_element()
                    }
                    None => div().text_xs().text_color(hsla(p.muted_foreground)).child("Publish result unavailable.").into_any_element(),
                }
            }
        };

        let footer = if draft.step == 2 {
            h_flex().justify_end().child(DialogButton::new("publish-done", "Done", true).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))))
        } else {
            let back_label = if draft.step == 0 { "Cancel" } else { "Back" };
            let primary = if draft.step == 0 {
                DialogButton::new("publish-next", "Next", true)
                    .disabled(!has_ready || !selected_readiness.ready)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(d) = this.publish.as_mut() {
                            d.step = 1;
                        }
                        this.sync_publish_prefill(window, cx);
                        this.publish_repo.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }))
            } else {
                DialogButton::new("publish-submit", if draft.pending { "Publishing..." } else { "Publish" }, true)
                    .disabled(!can_submit)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_publish(window, cx)))
            };
            h_flex()
                .justify_end()
                .gap(px(8.))
                .child(DialogButton::new("publish-back", back_label, false).disabled(draft.pending).on_click(cx.listener(|this, _, window, cx| {
                    if this.publish.as_ref().is_some_and(|d| d.step == 0) {
                        this.close(window, cx);
                    } else if let Some(d) = this.publish.as_mut() {
                        d.step = d.step.saturating_sub(1);
                        cx.notify();
                    }
                })))
                .child(primary)
        };

        let frame = Frame {
            id: "publish-repository-dialog",
            max_width: 520.,
            title: "Publish repository".into(),
            description: "Pick where to host it, then point us at a repo to push to.".into(),
            note: None,
            body: Some(v_flex().child(steps_bar).child(step_body).into_any_element()),
            footer: footer.into_any_element(),
        };
        frame.render(self.focus.clone(), cx)
    }

    fn render_checkout_pr(&self, draft: &CheckoutPrDraft, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let store = self.git.read(cx);
        let terms = terminology(store.status.as_ref().and_then(|s| s.provider_kind()));
        let provider_name = store
            .status
            .as_ref()
            .and_then(|s| s.local.source_control_provider.as_ref())
            .map(|sc| sc.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("GitHub");
        let raw = self.pr_input.read(cx).value().to_string();
        let parsed = parse_pull_request_reference(&raw);
        let validation = if !draft.dirty {
            None
        } else if raw.trim().is_empty() {
            Some(format!("Paste a {} URL, checkout command, or enter 123 / #123.", terms.singular))
        } else if parsed.is_none() {
            Some(format!("Use a {} URL, checkout command, 123, or #123.", terms.singular))
        } else {
            None
        };
        let error_message = validation
            .or_else(|| draft.prepare_error.clone())
            .or_else(|| draft.resolved.is_none().then(|| draft.resolve_error.clone()).flatten());
        let pending = draft.preparing.is_some();
        let can_write = store.can_write_source_control();
        let can_operate = store.can_operate_thread();
        let has_cwd = store.target.is_some();
        let can_local = can_write && has_cwd && draft.resolved.is_some() && !draft.resolving && !pending;
        let can_worktree = can_local && can_operate;

        let body = v_flex()
            .gap(px(12.))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(format!("{} reference", terms.short_label)))
                    .child(Input::new(&self.pr_input).disabled(pending)),
            )
            .children(draft.resolved.as_ref().map(|pr| {
                let tone = match pr.state.as_str() {
                    "merged" => hsla(p.primary),
                    "closed" => hsla(p.muted_foreground),
                    "open" => hsla(p.success),
                    _ => hsla(p.muted_foreground),
                };
                h_flex()
                    .gap(px(12.))
                    .items_start()
                    .p(px(12.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .bg(hsla(p.secondary))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(pr.title.clone()))
                            .child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(format!("#{} · {} to {}", pr.number, pr.head_branch, pr.base_branch))),
                    )
                    .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(tone).child(pr.state.clone()))
            }))
            .when(draft.resolving, |this| {
                this.child(
                    h_flex()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(Icon::LoaderCircle.sized(px(14.)))
                        .child(format!("Resolving {}...", terms.singular)),
                )
            })
            .children(error_message.map(|err| div().text_xs().text_color(hsla(p.destructive_foreground)).child(err)));

        let footer = h_flex()
            .justify_end()
            .gap(px(8.))
            .child(DialogButton::new("checkout-pr-cancel", "Cancel", false).disabled(pending).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))))
            .child(
                DialogButton::new(
                    "checkout-pr-local",
                    if draft.preparing == Some(PullRequestCheckoutMode::Local) { "Preparing local..." } else { "Local" },
                    false,
                )
                .disabled(!can_local)
                .on_click(cx.listener(|this, _, window, cx| this.confirm_checkout_pr(PullRequestCheckoutMode::Local, window, cx))),
            )
            .child(
                DialogButton::new(
                    "checkout-pr-worktree",
                    if draft.preparing == Some(PullRequestCheckoutMode::Worktree) { "Preparing worktree..." } else { "Worktree" },
                    true,
                )
                .disabled(!can_worktree)
                .on_click(cx.listener(|this, _, window, cx| this.confirm_checkout_pr(PullRequestCheckoutMode::Worktree, window, cx))),
            );

        let frame = Frame {
            id: "checkout-pr-dialog",
            max_width: 540.,
            title: format!("Checkout {}", terms.singular).into(),
            description: format!("Resolve a {provider_name} {}, then check it out in the main repo or in a dedicated worktree.", terms.singular).into(),
            note: None,
            body: Some(body.into_any_element()),
            footer: footer.into_any_element(),
        };
        frame.render(self.focus.clone(), cx)
    }
}

/// One Git action request (`RunGitActionWithToastInput`).
pub struct ActionRequest {
    pub action: StackedAction,
    pub commit_message: Option<String>,
    pub file_paths: Option<Vec<String>>,
    pub feature_branch: bool,
    pub skip_prompt: bool,
}

impl ActionRequest {
    pub fn new(action: StackedAction) -> Self {
        ActionRequest { action, commit_message: None, file_paths: None, feature_branch: false, skip_prompt: false }
    }
}

/// The dialog shell (design-system §6.4): scrim, dialog glass, a 24px header, an optional panel,
/// and the `bare` footer.
struct Frame {
    id: &'static str,
    max_width: f32,
    title: SharedString,
    description: SharedString,
    note: Option<SharedString>,
    body: Option<gpui::AnyElement>,
    footer: gpui::AnyElement,
}

impl Frame {
    fn render(self, focus: FocusHandle, cx: &mut Context<GitDialogs>) -> impl IntoElement + use<> {
    let Frame { id, max_width, title, description, note, body, footer } = self;
    let p = palette(cx);
    deferred(
        div()
            .id("git-dialog-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .id(id)
                    .track_focus(&focus)
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.close(window, cx);
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w_full()
                    .max_w(px(max_width))
                    .relative()
                    .rounded(px(16.))
                    .text_color(hsla(p.popover_foreground))
                    .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                    .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                    .child(
                        v_flex()
                            .p(px(24.))
                            .gap(px(8.))
                            .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(description))
                            .children(note.map(|note| div().text_xs().text_color(hsla(p.muted_foreground)).child(note))),
                    )
                    .children(body.map(|body| div().px(px(24.)).pt(px(4.)).pb(px(24.)).child(body)))
                    .child(div().px(px(24.)).py(px(16.)).child(footer)),
            ),
    )
    .with_priority(50)
    }
}

impl Render for GitDialogs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(pending) = self.pending.as_ref() {
            return self.render_pending(pending, cx).into_any_element();
        }
        if let Some(publish) = self.publish.as_ref() {
            return self.render_publish(publish, cx).into_any_element();
        }
        if let Some(checkout_pr) = self.checkout_pr.as_ref() {
            return self.render_checkout_pr(checkout_pr, cx).into_any_element();
        }
        match self.commit.as_ref() {
            Some(draft) => self.render_commit(draft, cx).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A dialog footer button (`button.tsx` `outline` / `default` at `sm-multiline`): 28 tall, it
/// wraps rather than truncating long labels such as "Commit, push & create PR".
#[derive(IntoElement)]
struct DialogButton {
    id: ElementId,
    label: SharedString,
    primary: bool,
    disabled: bool,
    on_click: Option<Handler>,
}

impl DialogButton {
    fn new(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Self {
        DialogButton { id: id.into(), label: label.into(), primary, disabled: false, on_click: None }
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for DialogButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let (fill, hover, text, border) = if self.primary {
            (hsla(p.primary), hsla(p.primary).opacity(0.9), hsla(p.primary_foreground), hsla(p.primary))
        } else if dark {
            (hsla(p.input).opacity(0.32), hsla(p.input).opacity(0.64), hsla(p.foreground), hsla(p.input))
        } else {
            (hsla(p.popover), hsla(p.accent).opacity(0.5), hsla(p.foreground), hsla(p.input))
        };
        let enabled = !self.disabled;
        focusable(div().id(self.id.clone()), self.id, enabled, window, cx)
            .min_h(px(28.))
            .px(px(9.))
            .py(px(3.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(text)
            .when(self.disabled, |this| this.opacity(0.64))
            .when(enabled, |this| this.cursor_pointer().hover(move |style| style.bg(hover)))
            .child(self.label)
            .when_some(self.on_click.filter(|_| enabled), |this, handler| this.on_click(move |event, window, cx| handler(event, window, cx)))
    }
}
