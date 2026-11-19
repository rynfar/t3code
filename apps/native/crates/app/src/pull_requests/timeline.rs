//! The Timeline tab (`PullRequestTimelineTab.tsx`): opened, commits, comments and reviews with
//! their review and overview threads, thread replies (`pullRequests.replyToThread`), inline
//! comment editing (`pullRequests.updateComment`), reactions (`PullRequestReactionBar`),
//! load-more replies (`pullRequests.threadComments`), thread resolution, and the comment form.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, list, prelude::*, px};
use gpui_component::input::Textarea;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::pull_requests::detail::{EventKind, TimelineEvent, build_timeline, change_request_term};
use t3_core::pull_requests::editing::{can_edit_comment, can_reply, can_resolve_thread};
use t3_core::pull_requests::reactions::can_react_comment;
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{Button, Size, Variant, avatar, diff_stat, outcome_glyph};
use super::detail::DetailView;
use super::interactions;
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::transcript::markdown::prose;

impl DetailView {
    /// Rebuilds the events once both the detail and its activity are in.
    pub(super) fn rebuild_timeline(&mut self) {
        let events = match (&self.detail, &self.activity) {
            (Some(detail), Some(activity)) => build_timeline(detail, activity, change_request_term(&detail.provider)),
            _ => Vec::new(),
        };
        if events != self.timeline {
            self.timeline_list.reset(events.len());
            self.timeline = events;
        }
    }
}

pub fn render(view: &mut DetailView, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    if view.activity.is_none() {
        let text = view.activity_error.clone().unwrap_or_else(|| "Loading activity…".to_owned());
        return div().flex_1().flex().items_center().justify_center().px(px(24.)).text_sm().text_color(hsla(p.muted_foreground)).child(text).into_any_element();
    }
    let truncated = view.activity.as_ref().filter(|activity| activity.comments_truncated).map(|activity| activity.comments.len());
    let composer = view.detail.clone().and_then(|detail| interactions::comment_form(view, &detail, window, cx));
    v_flex()
        .flex_1()
        .min_h_0()
        .children(truncated.map(|count| {
            let loaded = if count == 1 { "1 comment is".to_owned() } else { format!("{count} comments are") };
            div()
                .mx(px(16.))
                .mt(px(8.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.3))
                .bg(hsla(p.warning_surface))
                .text_xs()
                .child(format!("This conversation is only partially loaded. {loaded} loaded; open it on the host to read the rest."))
        }))
        .child(list(view.timeline_list.clone(), cx.processor(|this, index: usize, window, cx| event_row(this, index, window, cx))).flex_1())
        .children(composer)
        .into_any_element()
}

fn event_row(view: &mut DetailView, index: usize, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let Some(event) = view.timeline.get(index).cloned() else { return div().into_any_element() };
    v_flex().px(px(16.)).py(px(8.)).child(event_card(view, &event, false, window, cx)).into_any_element()
}

fn event_icon(event: &TimelineEvent, cx: &App) -> (Icon, gpui::Hsla) {
    let p = palette(cx);
    if let Some(outcome) = event.outcome {
        return outcome_glyph(outcome, cx);
    }
    match event.kind {
        EventKind::Opened => (Icon::GitPullRequestArrow, hsla(p.pill_completed)),
        EventKind::Commit => (Icon::GitCommit, hsla(p.muted_foreground)),
        EventKind::Comment | EventKind::Review => (Icon::MessageSquare, hsla(p.muted_foreground)),
        EventKind::Merged => (Icon::GitMerge, hsla(p.pill_plan)),
        EventKind::Closed => (Icon::GitPullRequestClosed, hsla(p.pill_failed)),
    }
}

pub(super) fn event_card(view: &mut DetailView, event: &TimelineEvent, reply: bool, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    let (icon, color) = event_icon(event, cx);
    let actor = event.actor.as_ref().map(|actor| actor.login.clone());
    let when = parse_iso(&event.at).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
    let title = match event.outcome {
        Some(outcome) => outcome.label().to_lowercase(),
        None => event.title.clone(),
    };
    let location = event.thread.as_ref().and_then(|thread| {
        let path = thread.path.as_ref()?;
        Some(match thread.line {
            Some(line) => format!("{path}:{line}"),
            None => path.clone(),
        })
    });
    let resolved = event.thread.as_ref().is_some_and(|thread| thread.is_resolved);
    let can_write = view.can_write_source_control(cx);
    let detail_clone = view.detail.clone();
    // On the thread's first comment only, as the web places it.
    let resolve = event
        .thread
        .as_ref()
        .filter(|thread| !reply && can_write && detail_clone.as_ref().is_some_and(|detail| can_resolve_thread(detail, thread.can_resolve)))
        .map(|thread| {
            let (id, resolved) = (thread.id.clone(), thread.is_resolved);
            Button::new(SharedString::from(format!("pr-resolve-{id}")), Variant::Ghost, Size::Xs)
                .label(if resolved { "Unresolve" } else { "Resolve" })
                .disabled(view.resolving.contains(&id))
                .on_click(cx.listener(move |this, _, _, cx| this.set_thread_resolution(id.clone(), !resolved, cx)))
        });
    let can_edit_this = can_write
        && event.comment_kind.is_some_and(|kind| {
            detail_clone.as_ref().is_some_and(|detail| can_edit_comment(detail, kind, event.can_edit, event.actor.as_ref()))
        });
    let is_editing_this = view.editing_comment.as_ref().is_some_and(|edit| edit.comment_id == event.id);
    let edit_button = (can_edit_this && !is_editing_this).then(|| {
        let id = event.id.clone();
        let kind = event.comment_kind.unwrap();
        let body = event.raw_body.clone().or_else(|| event.body.clone()).unwrap_or_default();
        Button::new(SharedString::from(format!("pr-edit-comment-{id}")), Variant::GhostMuted, Size::Xs)
            .icon(Icon::Pencil)
            .tooltip("Edit comment")
            .on_click(cx.listener(move |this, _, window, cx| this.start_comment_edit(id.clone(), kind, body.clone(), window, cx)))
    });
    let can_reply_here = !reply && can_write && event.thread.is_some() && detail_clone.as_ref().is_some_and(can_reply);
    let reply_thread_id = event.thread.as_ref().map(|t| t.id.clone());
    let is_replying_here = reply_thread_id.as_deref().is_some_and(|tid| view.replying_thread.as_deref() == Some(tid));
    let reply_button = (can_reply_here && !is_replying_here).then(|| {
        let tid = reply_thread_id.clone().unwrap();
        Button::new(SharedString::from(format!("pr-reply-open-{tid}")), Variant::GhostMuted, Size::Xs)
            .icon(Icon::Reply)
            .tooltip("Reply")
            .on_click(cx.listener(move |this, _, window, cx| this.start_thread_reply(tid.clone(), window, cx)))
    });

    let has_words = event.body.is_some() || event.thread.as_ref().is_some_and(|thread| thread.replies.iter().any(|reply| reply.body.is_some()));
    let add_to_chat = (!reply && matches!(event.kind, EventKind::Comment | EventKind::Review) && has_words && view.has_chat_target(cx)).then(|| {
        let thread_id = event.thread.as_ref().map(|thread| thread.id.clone());
        let comment_id = event.id.clone();
        Button::new(SharedString::from(format!("pr-add-chat-{}", event.id)), Variant::GhostMuted, Size::Xs)
            .icon(Icon::MessageSquarePlus)
            .tooltip("Add to chat")
            .on_click(cx.listener(move |this, _, _, cx| this.add_comment_to_chat(thread_id.as_deref(), &comment_id, cx)))
    });
    let scope = detail_clone.as_ref().map(|detail| view.media_scope(detail, cx));
    let body: Option<AnyElement> = if is_editing_this {
        let edit_input = view.editing_comment.as_ref().map(|e| e.input.clone());
        let saving = view.editing_comment.as_ref().is_some_and(|e| e.saving);
        edit_input.map(|input| {
            v_flex()
                .gap(px(6.))
                .child(Textarea::new(&input).disabled(saving))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(Button::new("pr-edit-comment-cancel", Variant::Ghost, Size::Xs).label("Cancel").disabled(saving).on_click(cx.listener(|this, _, _, cx| {
                            this.editing_comment = None;
                            cx.notify();
                        })))
                        .child(
                            Button::new("pr-edit-comment-save", Variant::Outline, Size::Xs)
                                .label(if saving { "Saving..." } else { "Save" })
                                .disabled(saving)
                                .on_click(cx.listener(|this, _, _, cx| this.commit_comment_edit(cx))),
                        ),
                )
                .into_any_element()
        })
    } else {
        event.body.clone().map(|body| {
            if event.markdown {
                let text: Arc<str> = Arc::from(body.as_str());
                let state = view.markdown.state_with_media_scope(&format!("event:{}", event.id), &text, None, scope.clone(), cx);
                prose(&state, cx).into_any_element()
            } else {
                div().text_sm().text_color(hsla(p.foreground)).child(body).into_any_element()
            }
        })
    };
    let can_react = matches!(event.kind, EventKind::Comment | EventKind::Review)
        && can_write
        && detail_clone.as_ref().is_some_and(|detail| can_react_comment(detail, event.comment_kind));
    let reactions_el = matches!(event.kind, EventKind::Comment | EventKind::Review)
        .then(|| interactions::reaction_bar(view, Some(&event.id), &event.reactions, can_react, cx))
        .flatten();

    let replies: Vec<TimelineEvent> = event.thread.as_ref().map(|thread| thread.replies.clone()).unwrap_or_default();
    let more = event.thread.as_ref().map_or(0, |thread| thread.more_replies);
    let next_cursor = event.thread.as_ref().and_then(|thread| thread.next_comments_cursor.clone());
    let replies: Vec<AnyElement> = replies.iter().map(|reply| event_card(view, reply, true, window, cx)).collect();
    let reply_composer = is_replying_here.then(|| {
        let tid = reply_thread_id.clone().unwrap();
        let input = view.ensure_reply_input(window, cx);
        let sending = view.reply_submitting;
        let empty = input.read(cx).value().trim().is_empty();
        v_flex()
            .gap(px(6.))
            .pt(px(4.))
            .child(Textarea::new(&input).disabled(sending))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(Button::new(SharedString::from(format!("pr-reply-cancel-{tid}")), Variant::Ghost, Size::Xs).label("Cancel").disabled(sending).on_click(cx.listener(|this, _, _, cx| {
                        this.replying_thread = None;
                        cx.notify();
                    })))
                    .child(
                        Button::new(SharedString::from(format!("pr-reply-send-{tid}")), Variant::Outline, Size::Xs)
                            .icon(Icon::Send)
                            .label(if sending { "Replying..." } else { "Reply" })
                            .disabled(sending || empty)
                            .on_click(cx.listener(move |this, _, window, cx| this.submit_thread_reply(tid.clone(), window, cx))),
                    ),
            )
    });
    let mono = cx.theme().mono_font_family.clone();
    let more_button = (more > 0).then(|| {
        let label = format!("{more} more {}", if more == 1 { "reply" } else { "replies" });
        if let (Some(tid), Some(cursor)) = (reply_thread_id.clone(), next_cursor) {
            let loading = view.loading_more_replies.contains(&tid);
            Button::new(SharedString::from(format!("pr-more-replies-{tid}")), Variant::GhostMuted, Size::Xs)
                .label(if loading { "Loading replies…".to_owned() } else { format!("Load {label}") })
                .disabled(loading)
                .on_click(cx.listener(move |this, _, _, cx| this.load_more_thread_replies(tid.clone(), cursor.clone(), cx)))
                .into_any_element()
        } else {
            div().text_xs().text_color(hsla(p.muted_foreground)).child(label).into_any_element()
        }
    });
    v_flex()
        .gap(px(6.))
        .child(
            h_flex()
                .gap(px(8.))
                .min_w_0()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(icon.sized(px(14.)).text_color(color))
                .children(actor.as_ref().map(|login| h_flex().flex_none().gap(px(4.)).child(avatar(login, 16., cx)).child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(login.clone()))))
                .child(div().min_w_0().truncate().child(title))
                .when(event.edited_at.is_some(), |this| this.child(div().flex_none().text_color(hsla(p.muted_foreground).opacity(0.75)).child("(edited)")))
                .when(event.additions.is_some() || event.deletions.is_some(), |this| this.child(diff_stat(event.additions.unwrap_or(0), event.deletions.unwrap_or(0), cx)))
                .child(div().flex_1())
                .when(resolved, |this| this.child(div().flex_none().child("Resolved")))
                .child(div().flex_none().whitespace_nowrap().child(when))
                .children(reply_button)
                .children(edit_button)
                .children(add_to_chat)
                .children(resolve),
        )
        .children(location.filter(|_| !reply).map(|location| div().font_family(mono).text_xs().text_color(hsla(p.muted_foreground)).child(location)))
        .children(body.map(|body| div().pl(px(22.)).child(body)))
        .children(reactions_el.map(|el| div().pl(px(22.)).child(el)))
        .when(!replies.is_empty() || more > 0 || reply_composer.is_some(), |this| {
            this.child(
                v_flex()
                    .ml(px(16.))
                    .pl(px(12.))
                    .gap(px(8.))
                    .border_l_2()
                    .border_color(hsla(p.border).opacity(0.6))
                    .children(replies)
                    .children(more_button)
                    .children(reply_composer),
            )
        })
        .into_any_element()
}
