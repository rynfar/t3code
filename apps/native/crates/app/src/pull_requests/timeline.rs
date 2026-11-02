//! The Timeline tab (`PullRequestTimelineTab.tsx`): opened, commits, comments and reviews with
//! their review threads, and the merge or close, newest first, in a virtualized list.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, list, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::pull_requests::detail::{EventKind, TimelineEvent, build_timeline, change_request_term};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{avatar, diff_stat, outcome_glyph};
use super::detail::DetailView;
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

pub fn render(view: &mut DetailView, _window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    if view.activity.is_none() {
        let text = view.activity_error.clone().unwrap_or_else(|| "Loading activity…".to_owned());
        return div().flex_1().flex().items_center().justify_center().px(px(24.)).text_sm().text_color(hsla(p.muted_foreground)).child(text).into_any_element();
    }
    let truncated = view.activity.as_ref().filter(|activity| activity.comments_truncated).map(|activity| activity.comments.len());
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
        .child(list(view.timeline_list.clone(), cx.processor(|this, index: usize, _, cx| event_row(this, index, cx))).flex_1())
        .into_any_element()
}

fn event_row(view: &mut DetailView, index: usize, cx: &mut Context<DetailView>) -> AnyElement {
    let Some(event) = view.timeline.get(index).cloned() else { return div().into_any_element() };
    v_flex().px(px(16.)).py(px(8.)).child(event_card(view, &event, false, cx)).into_any_element()
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

fn event_card(view: &mut DetailView, event: &TimelineEvent, reply: bool, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    let (icon, color) = event_icon(event, cx);
    let actor = event.actor.as_ref().map(|actor| actor.login.clone());
    let when = parse_iso(&event.at).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
    let title = match event.outcome {
        Some(outcome) => outcome.label().to_lowercase(),
        None => event.title.clone(),
    };
    let location = event.thread.as_ref().map(|thread| match thread.line {
        Some(line) => format!("{}:{line}", thread.path),
        None => thread.path.clone(),
    });
    let resolved = event.thread.as_ref().is_some_and(|thread| thread.is_resolved);
    let body = event.body.clone().map(|body| {
        if event.markdown {
            let text: Arc<str> = Arc::from(body.as_str());
            let state = view.markdown.state(&format!("event:{}", event.id), &text, cx);
            prose(&state, cx).into_any_element()
        } else {
            div().text_sm().text_color(hsla(p.foreground)).child(body).into_any_element()
        }
    });
    let replies: Vec<TimelineEvent> = event.thread.as_ref().map(|thread| thread.replies.clone()).unwrap_or_default();
    let more = event.thread.as_ref().map_or(0, |thread| thread.more_replies);
    let replies: Vec<AnyElement> = replies.iter().map(|reply| event_card(view, reply, true, cx)).collect();
    let mono = cx.theme().mono_font_family.clone();
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
                .when(event.additions.is_some() || event.deletions.is_some(), |this| this.child(diff_stat(event.additions.unwrap_or(0), event.deletions.unwrap_or(0), cx)))
                .child(div().flex_1())
                .when(resolved, |this| this.child(div().flex_none().child("Resolved")))
                .child(div().flex_none().whitespace_nowrap().child(when)),
        )
        .children(location.filter(|_| !reply).map(|location| div().font_family(mono).text_xs().text_color(hsla(p.muted_foreground)).child(location)))
        .children(body.map(|body| div().pl(px(22.)).child(body)))
        .when(!replies.is_empty() || more > 0, |this| {
            this.child(
                v_flex()
                    .ml(px(16.))
                    .pl(px(12.))
                    .gap(px(8.))
                    .border_l_2()
                    .border_color(hsla(p.border).opacity(0.6))
                    .children(replies)
                    .when(more > 0, |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!("{more} more {}", if more == 1 { "reply" } else { "replies" })))),
            )
        })
        .into_any_element()
}
