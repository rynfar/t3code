//! Interactive sub-views for `DetailView`:
//! - `reaction_bar` (`PullRequestReactions.tsx`)
//! - `reviewer_picker` (`PullRequestReviewerPicker.tsx`)
//! - `stack_popover` (`PullRequestStackPopover.tsx`)
//! - `review_form_panel` (`PullRequestReviewForm.tsx`)
//! - `comment_form` (`PullRequestCommentForm.tsx`)
//! - `thread_link_picker_modal` (`PullRequestThreadLinks.tsx` `ThreadPicker`)

use gpui::{
    AnyElement, App, BoxShadow, Context, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Input, Textarea};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::StateKind;
use t3_core::pull_requests::editing::{allowed_review_verdicts, can_comment, can_request_reviewers};
use t3_core::pull_requests::link::filter_linkable_threads;
use t3_core::pull_requests::reactions::{
    REACTION_ORDER, apply_pending_reactions, reaction_emoji, reaction_name, reaction_tooltip, reactions_signature,
};
use t3_core::pull_requests::review::{can_submit_review, review_verdict_label};
use t3_core::pull_requests::reviewers::reviewer_candidate_view;
use t3_core::pull_requests::wire::{Action, Detail, PrState, Reaction, ReviewerKind, Stack, StackMembership};

use super::controls::{Button, Size, Variant, anchored_picker, avatar, markdown_mode_buttons, markdown_preview};
use super::detail::DetailView;
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::theme::{ThemeSlot, ThemeSlotExt as _, hsla, palette};
use crate::components::motion::Spinner;

/// `PullRequestReactionBar`: shows reactions with optimistic pending overlay + "+" picker.
pub(super) fn reaction_bar(
    view: &DetailView,
    subject_id: Option<&str>,
    reactions: &[Reaction],
    can_react: bool,
    cx: &mut Context<DetailView>,
) -> Option<AnyElement> {
    let p = palette(cx);
    let subject_key = subject_id.unwrap_or("__pr__");
    let signature = reactions_signature(reactions);
    let pending_map = view.pending_reactions_for(subject_key, &signature);
    let shown = apply_pending_reactions(reactions, &pending_map);
    if shown.is_empty() && !can_react {
        return None;
    }
    let entity = cx.entity().downgrade();
    let subject_owned = subject_id.map(str::to_owned);
    let pills = shown.iter().map(|reaction| {
        let content = reaction.content;
        let reacted = reaction.viewer_has_reacted;
        let tooltip_text = reaction_tooltip(reaction);
        let subj = subject_owned.clone();
        let sig = signature.clone();
        div()
            .id(SharedString::from(format!("pr-react-{subject_key}-{}", content.as_str())))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(22.))
            .px(px(7.))
            .rounded_full()
            .border_1()
            .text_xs()
            .when(reacted, |this| this.border_color(hsla(p.primary).opacity(0.6)).bg(hsla(p.primary).opacity(0.12)).text_color(hsla(p.foreground)))
            .when(!reacted, |this| this.border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.muted).opacity(0.5)).text_color(hsla(p.muted_foreground)))
            .when(can_react, |this| {
                this.cursor_pointer().hover(|style| style.border_color(hsla(p.primary).opacity(0.7))).on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_reaction(subj.clone(), content, !reacted, sig.clone(), cx);
                }))
            })
            .child(reaction_emoji(content))
            .child(reaction.count.to_string())
            .tooltip(move |window, cx| tooltip_view(tooltip_text.clone().into(), window, cx))
    });
    let add_button = can_react.then(|| {
        let shown_clone = shown.clone();
        let subj = subject_owned.clone();
        let sig = signature.clone();
        Button::new(SharedString::from(format!("pr-react-add-{subject_key}")), Variant::GhostMuted, Size::Xs)
            .icon(Icon::Plus)
            .tooltip("Add a reaction")
            .dropdown_menu(move |mut menu, _, _| {
                for content in REACTION_ORDER {
                    let already = shown_clone.iter().any(|r| r.content == content && r.viewer_has_reacted);
                    let entity = entity.clone();
                    let subj = subj.clone();
                    let sig = sig.clone();
                    let label = format!("{}  {}", reaction_emoji(content), reaction_name(content));
                    menu = menu.item(PopupMenuItem::new(label).checked(already).on_click(move |_, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |view, cx| view.toggle_reaction(subj.clone(), content, !already, sig.clone(), cx));
                        }
                    }));
                }
                menu
            })
    });
    Some(h_flex().flex_wrap().gap(px(4.)).pt(px(2.)).children(pills).children(add_button).into_any_element())
}

/// `PullRequestReviewerPicker`: inline trigger + popover for searching and toggling reviewers.
pub(super) fn reviewer_picker(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> Option<AnyElement> {
    if !can_request_reviewers(detail) {
        return None;
    }
    let allowed = view.can_write_source_control(cx) && detail.viewer_permissions.request_reviewers;
    if !allowed {
        return Some(
            Button::new("pr-reviewer-add", Variant::GhostMuted, Size::Xs)
                .icon(Icon::UserPlus)
                .disabled(true)
                .tooltip("Asking someone to review needs write access on this repository")
                .into_any_element(),
        );
    }
    let open = view.reviewer_picker_open;
    let trigger = Button::new("pr-reviewer-add", Variant::GhostMuted, Size::Xs)
        .icon(Icon::UserPlus)
        .tooltip("Request a review")
        .on_click(cx.listener(|this, _, window, cx| this.toggle_reviewer_picker(window, cx)));
    if !open {
        return Some(trigger.into_any_element());
    }
    let search_input = view.ensure_reviewer_search_input(window, cx);
    let p = palette(cx);
    let searchable = detail.capabilities.reviewers.search_candidates == Some(true);
    let loaded = view.reviewer_candidates.as_ref().and_then(|r| r.as_ref().ok());
    let load_err = view.reviewer_candidates.as_ref().and_then(|r| r.as_ref().err());
    let search_data = view.reviewer_search_result.as_ref().and_then(|r| r.as_ref().ok());
    let search_err = view.reviewer_search_result.as_ref().and_then(|r| r.as_ref().err()).map(String::as_str);
    let reviewer_view = reviewer_candidate_view(
        loaded,
        searchable,
        &view.reviewer_query,
        &view.reviewer_debounced_query,
        search_data,
        view.reviewer_search_pending,
        search_err,
    );
    let pending_toggle = view.reviewer_request_pending.is_some();
    let list_body: AnyElement = if (view.reviewer_candidates.is_none() && load_err.is_none()) || reviewer_view.search_pending {
        h_flex().px(px(10.)).py(px(8.)).gap(px(6.)).text_xs().text_color(hsla(p.muted_foreground))
            .child(Spinner::new("pr-reviewers-loading", px(12.), hsla(p.muted_foreground)))
            .child(crate::i18n::t(cx, "Loading reviewers…")).into_any_element()
    } else if let Some(err) = load_err {
        h_flex()
            .justify_between()
            .px(px(10.))
            .py(px(8.))
            .gap(px(8.))
            .text_xs()
            .text_color(hsla(p.destructive))
            .child(format!("The people with access could not be read. {err}"))
            .child(Button::new("pr-reviewers-retry", Variant::Outline, Size::Xs).label("Retry").on_click(cx.listener(|this, _, _, cx| this.retry_reviewer_candidates(cx))))
            .into_any_element()
    } else if reviewer_view.candidates.is_empty() {
        let msg = if view.reviewer_query.trim().is_empty() {
            "Nobody else has access to this repository."
        } else {
            "Nobody with access matches that."
        };
        div().px(px(10.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child(msg).into_any_element()
    } else {
        v_flex()
            .max_h(px(220.))
            .overflow_y_scrollbar()
            .children(reviewer_view.candidates.into_iter().take(40).enumerate().map(|(ix, candidate)| {
                let c = candidate.clone();
                let is_team = candidate.kind == ReviewerKind::Team;
                let selected = view.is_reviewer_draft_selected(&candidate);
                h_flex()
                    .id(SharedString::from(format!("pr-reviewer-cand-{ix}")))
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .text_xs()
                    .when(!pending_toggle, |this| this.cursor_pointer().hover(|style| style.bg(hsla(p.accent))))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_reviewer(c.clone(), cx)))
                    .child(avatar(&candidate.login, 16., cx))
                    .child(div().flex_1().min_w_0().truncate().text_color(hsla(p.foreground)).child(candidate.login.clone()))
                    .when(is_team, |this| this.child(div().text_color(hsla(p.muted_foreground)).child("team")))
                    .when(selected, |this| this.child(Icon::Check.sized(px(13.)).text_color(hsla(p.primary))))
            }))
            .into_any_element()
    };
    let truncation_note = reviewer_view.truncation.map(|trunc| {
        div()
            .px(px(8.))
            .py(px(6.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .text_size(px(11.))
            .text_color(hsla(p.muted_foreground))
            .child(trunc.label(reviewer_view.search_error.as_deref()))
    });

    let content = v_flex().gap(px(4.))
        .child(h_flex().gap(px(6.))
            .child(div().flex_1().child(Input::new(&search_input).theme_slot(ThemeSlot::Input, cx)))
            .child(Button::new("pr-reviewer-close", Variant::GhostMuted, Size::Xs).icon(Icon::X)
                .tooltip(crate::i18n::t(cx, "Close"))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_reviewer_picker(window, cx)))))
        .child(list_body)
        .children(truncation_note);
    let view = cx.weak_entity();
    Some(anchored_picker("pr-reviewers-popup", trigger, content, 280., move |window, cx| {
        let _ = view.update(cx, |view, cx| {
            if view.reviewer_picker_open { view.toggle_reviewer_picker(window, cx); }
        });
    }, cx))
}

/// `PullRequestStackPopover`: shows `position/size` pill with layers icon and dropdown listing
/// all stack layers so the user can jump directly to any layer in the stack.
pub(super) fn stack_popover(
    membership: Option<&StackMembership>,
    stack: Option<&Result<Option<Stack>, String>>,
    current_number: u64,
    on_select_layer: impl Fn(u64, &mut App) + Clone + 'static,
    on_retry: impl Fn(&mut App) + Clone + 'static,
    id: SharedString,
    _cx: &App,
) -> Option<AnyElement> {
    let (label, tooltip) = match (membership, stack) {
        (Some(m), _) => (format!("{}/{}", m.position, m.size), format!("View stack #{}, layer {} of {}", m.number, m.position, m.size)),
        (None, Some(Ok(Some(stack)))) => {
            let pos = stack.layers.iter().position(|l| l.number == current_number).map_or(1, |i| i + 1);
            let size = stack.layers.len();
            (format!("{pos}/{size}"), format!("View stack #{}, layer {pos} of {size}", stack.number))
        }
        _ => return None,
    };
    let stack_clone = stack.cloned();
    Some(
        Button::new(id, Variant::GhostMuted, Size::Xs)
            .icon(Icon::Layers)
            .label(label)
            .tooltip(tooltip)
            .dropdown_menu(move |mut menu, _, cx| match &stack_clone {
                Some(Ok(Some(stack))) => {
                    menu = menu.item(PopupMenuItem::new(format!("Stack #{} → {}", stack.number, stack.base)).disabled(true)).separator();
                    for layer in &stack.layers {
                        let is_current = layer.number == current_number;
                        let kind = StateKind::of(layer.state, layer.is_draft == Some(true));
                        let title = layer.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(&layer.head_branch);
                        let item_label = format!("#{} {} ({})", layer.number, title, kind.label());
                        let num = layer.number;
                        let on_select = on_select_layer.clone();
                        menu = menu.item(PopupMenuItem::new(item_label).checked(is_current).on_click(move |_, _, cx| on_select(num, cx)));
                    }
                    menu
                }
                Some(Ok(None)) => menu.item(PopupMenuItem::new("This pull request is no longer in a stack.").disabled(true)),
                Some(Err(err)) => {
                    let on_retry = on_retry.clone();
                    menu.item(PopupMenuItem::new(format!("Stack error: {err}")).disabled(true))
                        .item(PopupMenuItem::new("Retry stack refresh").on_click(move |_, _, cx| on_retry(cx)))
                }
                None => {
                    let _ = cx;
                    menu.item(PopupMenuItem::new("Loading stack…").disabled(true))
                }
            })
            .into_any_element(),
    )
}

/// `PullRequestReviewForm`: verdict picker + review summary textarea + pending inline comments
/// list + Submit review button (`pullRequests.submitReview`).
pub(super) fn review_form_panel(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> Option<AnyElement> {
    if !view.review_form_open {
        return None;
    }
    let verdicts = allowed_review_verdicts(detail);
    if verdicts.is_empty() || !view.can_write_source_control(cx) {
        return None;
    }
    let p = palette(cx);
    let selected = view.review_draft.verdict.filter(|v| verdicts.contains(v)).unwrap_or(verdicts[0]);
    let summary_input = view.ensure_review_summary_input(window, cx);
    let summary_text = summary_input.read(cx).value().to_string();
    let comment_count = view.review_draft.comments.len();
    let request_changes_summary_required = detail.provider == "forgejo";
    let can_submit = !view.review_submitting && can_submit_review(selected, &summary_text, comment_count, request_changes_summary_required);
    let pending_comments = view.review_draft.comments.clone();
    let entity = cx.entity().downgrade();

    let verdict_control: Option<AnyElement> = (verdicts.len() > 1).then(|| {
        let verdicts_clone = verdicts.clone();
        Button::new("pr-review-verdict", Variant::Outline, Size::Xs)
            .label(review_verdict_label(selected))
            .disabled(view.review_submitting)
            .dropdown_menu(move |mut menu, _, _| {
                for verdict in &verdicts_clone {
                    let v = *verdict;
                    let entity = entity.clone();
                    menu = menu.item(PopupMenuItem::new(review_verdict_label(v)).checked(v == selected).on_click(move |_, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |view, cx| {
                                view.review_draft.verdict = Some(v);
                                cx.notify();
                            });
                        }
                    }));
                }
                menu
            })
            .into_any_element()
    });

    Some(
        v_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.muted).opacity(0.45))
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(if comment_count > 0 {
                        format!("Submit review ({comment_count} pending {})", if comment_count == 1 { "comment" } else { "comments" })
                    } else {
                        "Submit review".to_owned()
                    }))
                    .child(Button::new("pr-review-close", Variant::GhostMuted, Size::Xs).icon(Icon::X).on_click(cx.listener(|this, _, _, cx| {
                        this.review_form_open = false;
                        cx.notify();
                    }))),
            )
            .when(!pending_comments.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap(px(4.))
                        .max_h(px(140.))
                        .overflow_y_scrollbar()
                        .children(pending_comments.into_iter().enumerate().map(|(ix, draft)| {
                            let id = draft.id.clone();
                            h_flex()
                                .gap(px(6.))
                                .px(px(8.))
                                .py(px(5.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(hsla(p.border).opacity(0.7))
                                .bg(hsla(p.background))
                                .text_xs()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(format!("{} ({})", draft.path, draft.range_label)))
                                        .child(div().truncate().text_color(hsla(p.muted_foreground)).child(draft.body)),
                                )
                                .child(Button::new(SharedString::from(format!("pr-review-rm-{ix}")), Variant::GhostMuted, Size::Xs).icon(Icon::X).tooltip("Remove comment").on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_pending_review_comment(&id, cx);
                                })))
                        })),
                )
            })
            .child(Textarea::new(&summary_input).disabled(view.review_submitting))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .children(verdict_control)
                    .child(
                        Button::new("pr-review-submit", Variant::Default, Size::Xs)
                            .label(if view.review_submitting { "Submitting..." } else { "Submit review" })
                            .disabled(!can_submit)
                            .on_click(cx.listener(move |this, _, window, cx| this.submit_review(selected, window, cx))),
                    ),
            )
            .into_any_element(),
    )
}

/// `PullRequestCommentForm`: leave a top-level comment, optionally closing/reopening with comment.
pub(super) fn comment_form(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> Option<AnyElement> {
    if !can_comment(detail) || !view.can_write_source_control(cx) {
        return None;
    }
    let p = palette(cx);
    let input = view.ensure_comment_input(window, cx);
    let body = input.read(cx).value().to_string();
    let trimmed_empty = body.trim().is_empty();
    let locked = view.comment_submitting;
    let follow_up = if detail.state == PrState::Open && detail.can(Action::Close) {
        Some(Action::Close)
    } else if detail.state == PrState::Closed && detail.can(Action::Reopen) {
        Some(Action::Reopen)
    } else {
        None
    };
    let close_armed = view.comment_close_armed;

    let preview = view.comment_preview;
    let editor = if preview {
        markdown_preview(&body, &mut view.markdown, "pr-comment-preview", cx)
    } else {
        Textarea::new(&input).disabled(locked).theme_slot(ThemeSlot::Input, cx).into_any_element()
    };

    Some(
        v_flex()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.7))
            .child(markdown_mode_buttons("pr-comment-mode", preview, locked,
                cx.listener(move |view, _, window, cx| {
                    view.comment_preview = false;
                    input.update(cx, |input, cx| input.focus(window, cx));
                    cx.notify();
                }),
                cx.listener(|view, _, _, cx| { view.comment_preview = true; cx.notify(); }),
                cx,
            ))
            .child(editor)
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .children(follow_up.map(|action| {
                        let is_close = action == Action::Close;
                        let label = if locked {
                            if is_close { "Closing..." } else { "Reopening..." }
                        } else if is_close && close_armed {
                            "Confirm close?"
                        } else if is_close {
                            "Close with comment"
                        } else {
                            "Reopen with comment"
                        };
                        Button::new("pr-comment-followup", if is_close { Variant::DestructiveOutline } else { Variant::Outline }, Size::Xs)
                            .icon(if is_close { Icon::GitPullRequestClosed } else { Icon::GitPullRequestArrow })
                            .label(label)
                            .disabled(locked || trimmed_empty)
                            .on_click(cx.listener(move |this, _, window, cx| this.submit_comment_with_action(action, window, cx)))
                    }))
                    .child(
                        Button::new("pr-comment-post", Variant::Outline, Size::Xs)
                            .icon(Icon::Send)
                            .label(if locked { "Posting..." } else { "Comment" })
                            .disabled(locked || trimmed_empty)
                            .on_click(cx.listener(|this, _, window, cx| this.submit_comment(window, cx))),
                    ),
            )
            .into_any_element(),
    )
}

/// Inline thread-link picker modal (`PullRequestThreadLinks.tsx` `ThreadPicker`).
pub(super) fn thread_link_picker_modal(view: &mut DetailView, window: &mut Window, cx: &mut Context<DetailView>) -> Option<AnyElement> {
    if !view.thread_picker_open {
        return None;
    }
    let p = palette(cx);
    let input = view.ensure_thread_picker_input(window, cx);
    let query = input.read(cx).value().to_string();
    let linkable = view.linkable_threads(cx);
    let candidates = filter_linkable_threads(&linkable, &query);
    let pending = view.thread_link_pending;
    Some(
        deferred(
            div()
                .id("pr-thread-picker-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.thread_picker_open = false;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("pr-thread-picker-dialog")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(420.))
                        .max_w(px(480.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.popover))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.45),
                            offset: point(px(0.), px(16.)),
                            blur_radius: px(40.),
                            spread_radius: px(-12.),
                            inset: false,
                        }])
                        .p(px(12.))
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child("Link to thread"))
                                .child(Button::new("pr-thread-picker-close", Variant::GhostMuted, Size::Xs).icon(Icon::X).on_click(cx.listener(|this, _, _, cx| {
                                    this.thread_picker_open = false;
                                    cx.notify();
                                }))),
                        )
                        .child(Input::new(&input).disabled(pending))
                        .child(if candidates.is_empty() {
                            div().py(px(16.)).text_center().text_xs().text_color(hsla(p.muted_foreground)).child("No active threads found.").into_any_element()
                        } else {
                            v_flex()
                                .max_h(px(280.))
                                .overflow_y_scrollbar()
                                .gap(px(2.))
                                .children(candidates.into_iter().take(50).enumerate().map(|(ix, thread)| {
                                    let id = thread.id.clone();
                                    let linked = thread.already_linked;
                                    h_flex()
                                        .id(SharedString::from(format!("pr-thread-pick-{ix}")))
                                        .gap(px(8.))
                                        .px(px(8.))
                                        .py(px(6.))
                                        .rounded(px(6.))
                                        .when(!pending && !linked, |this| {
                                            this.cursor_pointer().hover(|style| style.bg(hsla(p.accent))).on_click(cx.listener(move |this, _, _, cx| {
                                                this.link_to_thread(id.clone(), false, cx);
                                            }))
                                        })
                                        .when(linked, |this| this.opacity(0.65))
                                        .child(Icon::MessageSquare.sized(px(14.)).text_color(hsla(p.muted_foreground)))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(if thread.title.trim().is_empty() { "Untitled thread".to_owned() } else { thread.title.clone() }))
                                                .child(div().truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(thread.project_title.clone())),
                                        )
                                        .when(linked, |this| {
                                            this.child(h_flex().gap(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).child(Icon::Check.sized(px(12.))).child("Linked"))
                                        })
                                }))
                                .into_any_element()
                        }),
                ),
        )
        .with_priority(55)
        .into_any_element(),
    )
}
