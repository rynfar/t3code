//! The Git dialogs (`GitActionsControl.tsx`): "Commit changes", with the files to include and an
//! optional message, and the default-branch confirmation that offers to continue on the branch or
//! check out a feature branch first. Also owns `runGitActionWithToast`'s confirmation check.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    App, BoxShadow, ClickEvent, Context, ElementId, Entity, FocusHandle, FontWeight, IntoElement, MouseButton, ParentElement, Render,
    RenderOnce, SharedString, Styled, Subscription, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::git_actions::{self, StackedAction, includes_commit, requires_default_branch_confirmation, terminology};
use t3_core::vcs::format_diff_count;

use super::{GitEvent, GitStore};
use crate::composer::controls::checkbox;
use crate::right_panel::{Button, Size, Variant, focusable};
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

pub struct GitDialogs {
    git: Entity<GitStore>,
    commit: Option<CommitDraft>,
    pending: Option<PendingDefault>,
    message: Entity<TextareaState>,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl GitDialogs {
    pub fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| TextareaState::new(window, cx).placeholder("Leave empty to auto-generate").auto_grow(3, 8));
        let subscription = cx.subscribe(&git, |this, _, _: &GitEvent, cx| {
            if this.commit.is_some() || this.pending.is_some() {
                cx.notify();
            }
        });
        GitDialogs { git, commit: None, pending: None, message, focus: cx.focus_handle(), _subscription: subscription }
    }

    /// `runGitActionWithToast`: pushing from the default branch asks first; anything else runs.
    pub fn request_action(&mut self, request: ActionRequest, window: &mut Window, cx: &mut Context<Self>) {
        let ActionRequest { action, commit_message, file_paths, feature_branch, skip_prompt } = request;
        let status = self.git.read(cx).status.clone();
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
        if feature_branch && self.feature_branch_unavailable(cx).is_some() {
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
        self.commit = Some(CommitDraft::default());
        self.message.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.commit = None;
        self.pending = None;
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
        let file_rows = files.iter().enumerate().map(|(index, file)| {
            let excluded = draft.excluded.contains(&file.path);
            let path = file.path.clone();
            h_flex()
                .id(ElementId::Name(format!("commit-file-{index}").into()))
                .w_full()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .hover(|style| style.bg(hsla(p.accent).opacity(0.5)))
                .when(editing, |this| {
                    this.cursor_pointer().child(checkbox(!excluded, cx)).on_click(cx.listener(move |this, _, _, cx| this.toggle_file(&path, cx)))
                })
                .child(div().flex_1().min_w_0().truncate().when(excluded, |this| this.text_color(hsla(p.muted_foreground))).child(file.path.clone()))
                .child(if excluded { div().flex_none().text_color(hsla(p.muted_foreground)).child("Excluded").into_any_element() } else { stat(file.insertions, file.deletions).into_any_element() })
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
        let footer = h_flex()
            .justify_end()
            .gap(px(8.))
            .child(DialogButton::new("commit-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(DialogButton::new("commit-new-branch", "Commit on new branch", false).disabled(none_selected).on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))))
            .child(DialogButton::new("commit-confirm", "Commit", true).disabled(none_selected).on_click(cx.listener(|this, _, window, cx| this.commit(false, window, cx))));
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
        let terms = terminology(self.git.read(cx).status.as_ref().and_then(|status| status.provider_kind()));
        let copy = git_actions::default_branch_dialog_copy(pending.action, &pending.branch, pending.includes_commit, terms);
        let unavailable = self.feature_branch_unavailable(cx);
        let footer = h_flex()
            .flex_wrap()
            .gap(px(8.))
            .child(div().mr_auto().child(DialogButton::new("default-branch-abort", "Abort", false).on_click(cx.listener(|this, _, _, cx| this.close(cx)))))
            .child(DialogButton::new("default-branch-continue", copy.continue_label, false).on_click(cx.listener(|this, _, window, cx| this.continue_pending(false, window, cx))))
            .child(
                DialogButton::new("default-branch-feature", "Check out feature branch & continue", true)
                    .disabled(unavailable.is_some())
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
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                div()
                    .id(id)
                    .track_focus(&focus)
                    .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                        this.close(cx);
                        cx.stop_propagation();
                    }))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            this.close(cx);
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
