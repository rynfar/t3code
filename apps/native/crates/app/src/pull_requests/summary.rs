//! The Summary tab (`PullRequestSummaryTab.tsx`): reviewers with the reviewer picker
//! (`PullRequestReviewerPicker.tsx`), labels with the label picker (`PullRequestLabelPicker.tsx`),
//! the description as markdown with an inline text editor and task checkboxes that save back to
//! the host (`saveDescription`), maintainer edits toggle, reactions (`PullRequestReactionBar`),
//! media read through the pull request's own repository (`sourceControlMediaRepository`), the
//! collapsible Checks section, and comments with the new-comment composer.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::input::Textarea;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::{EventKind, check_status_label, describe_checks, ordered_checks};
use t3_core::pull_requests::editing::can_edit_maintainer_edits;
use t3_core::pull_requests::list::label_color;
use t3_core::pull_requests::tasks::{Segment, task_segments};
use t3_core::pull_requests::wire::Detail;

use super::controls::{BadgeKind, Button, Size, Variant, avatar, badge, check_glyph};
use super::detail::DetailView;
use super::interactions;
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::theme::{ThemeSlot, ThemeSlotExt as _, hsla, palette};
use crate::components::motion::Spinner;
use super::controls::{anchored_picker, markdown_mode_buttons, markdown_preview};
use crate::transcript::markdown::prose;

/// The indent of one level of list nesting in a split description.
const NEST: f32 = 20.;

pub fn render(view: &mut DetailView, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let Some(detail) = view.detail.clone() else { return div().into_any_element() };
    let p = palette(cx);
    let open = view.checks_open;
    if detail.capabilities.labels == Some(true) && view.label_candidates.is_none() {
        view.load_label_candidates(cx);
    }
    let can_edit_desc = view.can_edit_description(cx);
    let editing_desc = view.description_editing;
    let desc_heading = h_flex()
        .justify_between()
        .pr(px(16.))
        .child(section_heading(crate::i18n::t(cx, "Description"), None, cx))
        .when(can_edit_desc && !editing_desc, |this| {
            this.child(
                Button::new("pr-desc-edit", Variant::GhostMuted, Size::Xs)
                    .icon(Icon::Pencil)
                    .label(crate::i18n::t(cx, "Edit"))
                    .on_click(cx.listener(|this, _, window, cx| this.start_description_edit(window, cx))),
            )
        });
    let description = description(view, &detail, window, cx);
    let pr_reactions = view.activity.as_ref().and_then(|activity| {
        let can_react = view.can_write_source_control(cx) && detail.capabilities.reactions == Some(true);
        interactions::reaction_bar(view, None, &activity.reactions, can_react, cx)
    });
    let maintainer_edits = (can_edit_maintainer_edits(&detail) && view.can_write_source_control(cx)).then(|| {
        let enabled = detail.maintainer_can_modify == Some(true);
        let saving = view.maintainer_edits_saving;
        h_flex()
            .gap(px(8.))
            .px(px(16.))
            .pb(px(8.))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(
                Button::new("pr-maintainer-edits", Variant::Ghost, Size::Xs)
                    .icon(if enabled { Icon::Check } else { Icon::Square })
                    .label("Allow edits by maintainers")
                    .disabled(saving)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_maintainer_edits(!enabled, cx))),
            )
    });
    let comments = comments_section(view, &detail, window, cx);
    let reviewers_row = reviewers(view, &detail, window, cx);
    v_flex()
        .id("pr-summary")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&view.summary_scroll)
        .child(
            v_flex()
                .gap(px(8.))
                .px(px(16.))
                .pt(px(10.))
                .pb(px(4.))
                .child(reviewers_row)
                .when(!detail.labels.is_empty() || detail.capabilities.labels == Some(true), |this| this.child(labels(view, &detail, cx))),
        )
        .child(desc_heading)
        .child(
            v_flex()
                .px(px(16.))
                .pb(px(12.))
                .gap(px(8.))
                .child(description)
                .children(pr_reactions),
        )
        .children(maintainer_edits)
        .child(section_heading(crate::i18n::t(cx, "Checks"), Some(open), cx).id("pr-checks-toggle").cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
            this.checks_open = !this.checks_open;
            cx.notify();
        })))
        .children(open.then(|| checks(view, &detail, cx)))
        .when(!open, |this| this.child(div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(describe_checks(&detail.checks))))
        .child(comments)
        .into_any_element()
}

/// The description: inline editor while editing, else markdown (with writable task checkboxes).
fn description(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    if view.description_editing {
        let input = view.ensure_description_input(window, cx);
        let saving = view.description_saves > 0;
        let preview = view.description_preview;
        let editor = if preview {
            let raw = input.read(cx).value().to_string();
            let text = if detail.provider == "github" {
                t3_core::pull_requests::emoji::replace_github_emojis(&raw)
            } else {
                raw
            };
            markdown_preview(&text, &mut view.markdown, "pr-description-preview", cx)
        } else {
            Textarea::new(&input).disabled(saving).theme_slot(ThemeSlot::Input, cx).into_any_element()
        };
        let focus = input.clone();
        return v_flex()
            .gap(px(8.))
            .child(markdown_mode_buttons("pr-description-mode", preview, saving,
                cx.listener(move |view, _, window, cx| {
                    view.description_preview = false;
                    focus.update(cx, |input, cx| input.focus(window, cx));
                    cx.notify();
                }),
                cx.listener(|view, _, _, cx| { view.description_preview = true; cx.notify(); }),
                cx,
            ))
            .child(editor)
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(Button::new("pr-desc-cancel", Variant::Ghost, Size::Xs).label(crate::i18n::t(cx, "Cancel")).disabled(saving).on_click(cx.listener(|this, _, _, cx| {
                        this.description_editing = false;
                        cx.notify();
                    })))
                    .child(
                        Button::new("pr-desc-save", Variant::Outline, Size::Xs)
                            .label(crate::i18n::t(cx, if saving { "Saving..." } else { "Save" }))
                            .disabled(saving)
                            .on_click(cx.listener(|this, _, _, cx| this.commit_description_edit(cx))),
                    ),
            )
            .into_any_element();
    }
    let body = view.description().unwrap_or_default();
    let segments = if view.can_edit_description(cx) { task_segments(&body) } else { None };
    let scope = view.media_scope(detail, cx);
    let action_ctx = view.markdown_action_ctx(cx);
    let is_github = detail.provider == "github";
    let Some(segments) = segments else {
        let text: Arc<str> = if body.trim().is_empty() {
            Arc::from("_No description provided._")
        } else if is_github {
            Arc::from(t3_core::pull_requests::emoji::replace_github_emojis(&body).as_str())
        } else {
            Arc::from(body.as_str())
        };
        let state = view.markdown.state_with_media_scope(&format!("body:{}", detail.url), &text, Some(action_ctx), Some(scope), cx);
        return prose(&state, cx).into_any_element();
    };
    let p = palette(cx);
    let mut rows: Vec<AnyElement> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.into_iter().enumerate() {
        let (lead, text, depth): (Option<AnyElement>, String, usize) = match segment {
            Segment::Markdown { text, depth } => (None, text, depth),
            Segment::Task { marker, checked, text, depth } => (Some(checkbox(index, marker, checked, cx)), text, depth),
            Segment::Item { bullet, text, depth } => (Some(div().flex_none().min_w(px(16.)).text_color(hsla(p.muted_foreground)).child(bullet).into_any_element()), text, depth),
        };
        let rendered = if is_github {
            t3_core::pull_requests::emoji::replace_github_emojis(&text)
        } else {
            text
        };
        let text: Arc<str> = Arc::from(rendered.as_str());
        let state = view.markdown.state_with_media_scope(&format!("body:{}:{index}", detail.url), &text, Some(action_ctx.clone()), Some(scope.clone()), cx);
        let content = div().flex_1().min_w_0().child(prose(&state, cx));
        rows.push(h_flex().items_start().gap(px(6.)).pl(px(NEST * depth as f32)).children(lead).child(content).into_any_element());
    }
    v_flex().gap(px(4.)).children(rows).into_any_element()
}

/// A task's checkbox, drawn as the markdown view draws one; read-only when its marker was not
/// found in the source.
fn checkbox(index: usize, marker: Option<usize>, checked: bool, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    div()
        .id(SharedString::from(format!("pr-task-{index}")))
        .flex_none()
        .mt(px(5.))
        .size(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(hsla(p.primary))
        .text_color(hsla(p.primary_foreground))
        .when(checked, |this| this.bg(hsla(p.primary)).child(Icon::Check.sized(px(10.))))
        .when_some(marker, |this, marker| {
            this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_task(marker, !checked, cx);
            }))
        })
        .into_any_element()
}

fn section_heading(title: impl Into<SharedString>, open: Option<bool>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    h_flex()
        .gap(px(6.))
        .px(px(16.))
        .py(px(12.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(title.into())
        .children(open.map(|open| (if open { Icon::ChevronDown } else { Icon::ChevronRight }).sized(px(14.)).text_color(hsla(p.muted_foreground).opacity(0.6))))
}

/// `MetaRow`: a 96px label column, then the value.
fn meta_row(icon: Icon, label: &'static str, value: impl IntoElement, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    h_flex()
        .items_start()
        .min_h(px(24.))
        .gap(px(8.))
        .text_xs()
        .child(h_flex().w(px(96.)).flex_none().gap(px(6.)).pt(px(2.)).text_color(hsla(p.muted_foreground)).child(icon.sized(px(14.))).child(label))
        .child(div().flex_1().min_w_0().text_color(hsla(p.foreground)).child(value))
}

fn reviewers(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    let picker = interactions::reviewer_picker(view, detail, window, cx);
    let value = h_flex()
        .flex_wrap()
        .items_start()
        .gap(px(6.))
        .when(detail.reviewers.is_empty(), |this| this.child(div().pt(px(2.)).text_color(hsla(p.muted_foreground)).child("None")))
        .children(detail.reviewers.iter().map(|reviewer| {
            let named = match &reviewer.name {
                Some(name) if *name != reviewer.login => format!("{name} (@{})", reviewer.login),
                _ => reviewer.login.clone(),
            };
            h_flex()
                .id(SharedString::from(format!("pr-reviewer-{}", reviewer.login)))
                .gap(px(4.))
                .pt(px(2.))
                .child(avatar(&reviewer.login, 16., cx))
                .child(reviewer.login.clone())
                .tooltip(move |window, cx| tooltip_view(named.clone().into(), window, cx))
        }))
        .children(picker);
    meta_row(Icon::Users, "Reviewers", value, cx).into_any_element()
}

fn labels(view: &DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> impl IntoElement {
    let p = palette(cx);
    let applied = h_flex()
        .flex_wrap()
        .gap(px(4.))
        .when(detail.labels.is_empty(), |this| this.child(div().text_color(hsla(p.muted_foreground)).child("None")))
        .children(detail.labels.iter().map(|label| badge(label.name.clone(), label_color(label.color.as_deref()).map_or(BadgeKind::Secondary, BadgeKind::Label), true, cx)))
        .when(detail.capabilities.labels == Some(true), |this| this.child(label_picker(view, detail, cx)));
    meta_row(Icon::Tag, "Labels", applied, cx)
}

/// `PullRequestLabelPicker` (`pingdotgg/t3code#37733`): toggles multiple labels in a local draft
/// and commits all additions and removals in one batch when the picker closes.
fn label_picker(view: &DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> AnyElement {
    let allowed = view.can_write_source_control(cx) && detail.viewer_permissions.labels != Some(false);
    if !allowed {
        return Button::new("pr-labels-edit", Variant::GhostMuted, Size::Xs)
            .icon(Icon::Tag)
            .disabled(true)
            .tooltip("Changing labels needs triage access on this repository")
            .into_any_element();
    }
    let open = view.label_picker_open;
    let trigger = Button::new("pr-labels-edit", Variant::GhostMuted, Size::Xs)
        .icon(Icon::Tag)
        .tooltip(if open { "Apply label changes" } else { "Change labels" })
        .on_click(cx.listener(|this, _, _, cx| this.toggle_label_picker(cx)));
    if !open {
        return trigger.into_any_element();
    }
    let p = palette(cx);
    let pending = view.label_pending;
    let body: AnyElement = match &view.label_candidates {
        None => h_flex().px(px(10.)).py(px(8.)).gap(px(6.)).text_xs().text_color(hsla(p.muted_foreground))
            .child(Spinner::new("pr-labels-loading", px(12.), hsla(p.muted_foreground)))
            .child(crate::i18n::t(cx, "Loading labels…")).into_any_element(),
        Some(Err(_)) => h_flex()
            .justify_between()
            .px(px(10.))
            .py(px(8.))
            .gap(px(8.))
            .text_xs()
            .text_color(hsla(p.destructive))
            .child("The labels could not be read.")
            .child(Button::new("pr-labels-retry", Variant::Outline, Size::Xs).label("Retry").on_click(cx.listener(|this, _, _, cx| this.retry_label_candidates(cx))))
            .into_any_element(),
        Some(Ok(list)) if list.candidates.is_empty() => div().px(px(10.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child("This repository has no labels.").into_any_element(),
        Some(Ok(list)) => {
            let truncated = list.truncated;
            v_flex()
                .child(
                    v_flex()
                        .max_h(px(220.))
                        .overflow_y_scrollbar()
                        .children(list.candidates.iter().enumerate().map(|(ix, candidate)| {
                            let selected = view.is_label_draft_selected(candidate);
                            let c = candidate.clone();
                            let desc = candidate.description.as_deref().filter(|text| !text.is_empty()).map(str::to_owned);
                            h_flex()
                                .id(SharedString::from(format!("pr-label-cand-{ix}")))
                                .gap(px(6.))
                                .px(px(8.))
                                .py(px(5.))
                                .rounded(px(6.))
                                .text_xs()
                                .when(!pending, |this| this.cursor_pointer().hover(|style| style.bg(hsla(p.accent))))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_label_draft(c.clone(), cx)))
                                .child(div().flex_1().min_w_0().truncate().text_color(hsla(p.foreground)).child(match desc {
                                    Some(text) => format!("{} · {text}", candidate.name),
                                    None => candidate.name.clone(),
                                }))
                                .when(selected, |this| this.child(Icon::Check.sized(px(13.)).text_color(hsla(p.primary))))
                        })),
                )
                .when(truncated, |this| {
                    this.child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .border_t_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .text_size(px(11.))
                            .text_color(hsla(p.muted_foreground))
                            .child("This repository has more labels than are listed here. Apply the rest on the host."),
                    )
                })
                .into_any_element()
        }
    };
    let content = v_flex().gap(px(4.)).child(
        h_flex().justify_between().px(px(6.)).py(px(2.))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, "Labels")))
            .child(Button::new("pr-labels-done", Variant::Ghost, Size::Xs)
                .label(crate::i18n::t(cx, "Done"))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_label_picker(cx)))),
    ).child(body);
    let view = cx.weak_entity();
    anchored_picker("pr-labels-popup", trigger, content, 260., move |_, cx| {
        let _ = view.update(cx, |view, cx| {
            if view.label_picker_open { view.toggle_label_picker(cx); }
        });
    }, cx)
}

fn checks(view: &DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> impl IntoElement {
    let p = palette(cx);
    if detail.checks.is_empty() {
        return v_flex()
            .px(px(16.))
            .pb(px(16.))
            .gap(px(8.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("No checks reported."))
            .child(
                div().child(Button::new("pr-checks-refresh", Variant::Ghost, Size::Xs).label("Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh_checks(cx)))),
            );
    }
    let state = view.state.clone();
    let thread_id = view.link_thread_id(cx);
    v_flex().px(px(12.)).pb(px(16.)).children(ordered_checks(&detail.checks).into_iter().enumerate().map(move |(index, check)| {
        let (icon, color) = check_glyph(check.status, cx);
        let url = check.url.clone();
        let state = state.clone();
        let thread_id = thread_id.clone();
        h_flex()
            .id(SharedString::from(format!("pr-check-{index}")))
            .items_start()
            .gap(px(8.))
            .px(px(8.))
            .py(px(8.))
            .rounded(px(6.))
            .text_xs()
            .line_height(px(20.))
            .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
            .when(url.is_some(), |this| this.cursor_pointer())
            .child(div().mt(px(3.)).child(if icon == Icon::LoaderCircle {
                Spinner::new(("pr-check-spin", index), px(14.), color).into_any_element()
            } else {
                icon.sized(px(14.)).text_color(color).into_any_element()
            }))
            .child(div().flex_1().min_w_0().text_color(hsla(p.foreground)).child(check.name.clone()))
            .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(check_status_label(check)))
            .on_click(move |ev, _, cx| {
                if let Some(url) = &url {
                    crate::transcript::markdown::open_browser_link(
                        url,
                        ev.modifiers(),
                        Some(&state),
                        thread_id.clone(),
                        cx,
                    );
                }
            })
    }))
}

/// Conversation comments and review/overview threads on Summary, plus the comment composer.
fn comments_section(view: &mut DetailView, detail: &Detail, window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    let Some(activity) = view.activity.as_ref() else {
        let heading = section_heading("Comments", None, cx);
        let text = view.activity_error.clone().unwrap_or_else(|| "Loading activity…".to_owned());
        return v_flex()
            .child(heading)
            .child(div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(text))
            .into_any_element();
    };
    let count = activity.comment_count.max(activity.comments.len() as u64);
    let truncated = activity.comments_truncated.then_some(activity.comments.len());
    let items: Vec<_> = view.timeline.iter().filter(|event| matches!(event.kind, EventKind::Comment | EventKind::Review)).cloned().collect();
    let heading = section_heading(format!("Comments ({count})"), None, cx);
    let body = if items.is_empty() {
        div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child("No comments yet.").into_any_element()
    } else {
        let cards: Vec<AnyElement> = items.iter().map(|event| super::timeline::event_card(view, event, false, window, cx)).collect();
        v_flex().px(px(16.)).pb(px(16.)).gap(px(12.)).children(cards).into_any_element()
    };
    let composer = interactions::comment_form(view, detail, window, cx);
    v_flex()
        .child(heading)
        .children(truncated.map(|loaded_count| {
            let loaded = if loaded_count == 1 { "1 comment is".to_owned() } else { format!("{loaded_count} comments are") };
            div()
                .mx(px(16.))
                .mb(px(8.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.3))
                .bg(hsla(p.warning_surface))
                .text_xs()
                .child(format!("This conversation is only partially loaded. {loaded} loaded; open it on the host to read the rest."))
        }))
        .child(body)
        .children(composer)
        .into_any_element()
}
