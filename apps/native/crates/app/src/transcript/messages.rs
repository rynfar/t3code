//! Message rows: the user bubble (`TimelineUserMessageRow.tsx`, §7.5), assistant prose and its
//! meta footer (`TimelineAssistantRows.tsx`, §7.6) and the proposed plan card
//! (`ProposedPlanCard.tsx`).

use std::sync::Arc;

use gpui::{AnyElement, App, ClipboardItem, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, linear_color_stop, linear_gradient, prelude::FluentBuilder, px, relative};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use t3_core::media::MediaPlacement;
use t3_core::timeline::{EDIT_FROM_HERE, Attribution, ChatMessage, FORK_LABEL, can_fork, ProposedPlan, RowKind, TimelineRow, plan_title, should_collapse_user_message, strip_displayed_plan_markdown, user_message_attribution};

use super::markdown::{MarkdownStore, markdown_view, prose};
use super::parts::{ROW_GROUP, Clock, DividerTone, ghost_icon_button, ghost_text_button, system_divider, tooltip_icon_button};
use super::RowEnv;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;

const COLLAPSED_HEIGHT: f32 = 176.;

/// Single newlines are line breaks in a user message (`lineBreaks`), so they become hard breaks.
fn with_line_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut fenced = false;
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push_str(if fenced { "\n" } else { "  \n" });
        }
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        out.push_str(line);
    }
    out
}

fn model_label(selection: &Value) -> String {
    selection.get("model").and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `ModelSwitchRow`: what changed between the previous turn's model and this one.
fn model_switch(value: &Value, cx: &App) -> Option<AnyElement> {
    let (from, to) = (value.get("from")?, value.get("to")?);
    let (from_model, to_model) = (model_label(from), model_label(to));
    let effort = |selection: &Value| {
        selection.get("options").and_then(Value::as_array).and_then(|options| {
            options.iter().find_map(|option| {
                let id = option.get("id").and_then(Value::as_str)?;
                matches!(id, "reasoningEffort" | "effort" | "reasoning" | "variant" | "reasoning_effort" | "thinking_level")
                    .then(|| option.get("value").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())))
                    .flatten()
            })
        })
    };
    let (from_effort, to_effort) = (effort(from), effort(to));
    let model_changed = from_model != to_model;
    if !model_changed && from_effort == to_effort {
        return None;
    }
    let describe = |model: &str, effort: Option<&String>| {
        [model_changed.then_some(model), effort.map(String::as_str)].into_iter().flatten().collect::<Vec<_>>().join(" ")
    };
    let label = format!("{} → {}", describe(&from_model, from_effort.as_ref()), describe(&to_model, to_effort.as_ref()));
    Some(system_divider(label.into(), None, None, DividerTone::Neutral, cx))
}

pub fn user_row(row: &TimelineRow, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::Message { message, grouped_with_next, grouped_with_previous, continues_steer, .. } = &row.kind else {
        return div().into_any_element();
    };
    let (id, grouped_with_next, grouped_with_previous, continues_steer) = (row.id.as_str(), *grouped_with_next, *grouped_with_previous, *continues_steer);
    let p = palette(cx);
    let attribution = user_message_attribution(message);
    let text: Arc<str> = match &message.meta.guest_text {
        Some(text) => Arc::from(text.as_str()),
        None => message.text.clone(),
    };
    let context_item = message.item.clone();
    let records: &[Value] = match context_item.as_ref().map(|item| &item.kind) {
        Some(t3_core::turn_item::TurnItemKind::UserMessage { context: Some(context), .. }) => context["records"].as_array().map(Vec::as_slice).unwrap_or_default(),
        _ => &[],
    };
    let presentation = markdown.user_context(id, &text);
    let context_chips = super::context::chips(id, &presentation, records, context_item.as_ref(), env.state, env.ui, cx);
    let display_text = presentation.display_text.as_ref();
    let collapsible = should_collapse_user_message(display_text);
    let expanded = env.ui.expanded_messages.contains(id);
    let collapsed = collapsible && !expanded;
    let surface = hsla(if attribution == Some(Attribution::Guest) { p.guest_surface } else { p.message_surface });
    let body = if display_text.trim().is_empty() {
        None
    } else {
        let source: Arc<str> = Arc::from(with_line_breaks(display_text));
        let state = markdown.state(&format!("user:{id}"), &source, cx);
        Some(markdown_view(&state, cx))
    };
    let attachments: &[Value] = match message.item.as_ref().map(|item| &item.kind) {
        Some(t3_core::turn_item::TurnItemKind::UserMessage { attachments, .. }) => attachments,
        _ => &[],
    };
    let images = t3_core::media::image_attachments(attachments);
    let other_files = t3_core::media::other_attachments(attachments);
    let inline_media = matches!(attribution, Some(Attribution::Guest | Attribution::Agent));
    let placement = t3_core::media::media_placement(images.len(), body.is_none(), other_files.len(), inline_media);
    let media = placement.map(|placement| super::media::user_media(id, &images, placement, grouped_with_previous, grouped_with_next, cx));
    let (media_above, media_in_bubble) = match placement {
        Some(MediaPlacement::InBubble) => (None, media),
        _ => (media, None),
    };
    let small = |radius: bool| if radius { px(6.) } else { px(16.) };
    let media_alone = placement == Some(MediaPlacement::Alone);
    let bubble = v_flex()
        .relative()
        .min_w_0()
        .max_w(relative(0.8))
        .p(px(12.))
        .rounded_tl(px(16.))
        .rounded_bl(px(16.))
        .rounded_tr(small(grouped_with_previous || placement == Some(MediaPlacement::AboveText)))
        .rounded_br(small(grouped_with_next))
        .bg(surface)
        .text_size(px(14.))
        .line_height(px(22.75))
        .text_color(hsla(p.message_foreground))
        .children(context_chips)
        .children(media_in_bubble)
        .children(super::media::other_files(&other_files, cx))
        .children(body.map(|body| {
            div()
                .relative()
                .min_w_0()
                .when(collapsed, |this| this.max_h(px(COLLAPSED_HEIGHT)).overflow_hidden())
                .child(body)
                .when(collapsed, |this| {
                    this.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(28.))
                            .bg(linear_gradient(180., linear_color_stop(surface.opacity(0.), 0.), linear_color_stop(surface, 1.))),
                    )
                })
        }))
        .when(collapsible, |this| {
            let key = id.to_owned();
            this.child(h_flex().mt(px(6.)).justify_end().child(ghost_text_button(
                SharedString::from(format!("collapse:{id}")),
                if expanded { "Show less" } else { "Show full message" },
                None,
                cx.listener(move |view, _, _, cx| view.toggle_message(&key, cx)),
                cx,
            )))
        });
    let copy_text = text.to_string();
    let copy_message_id = message.id.clone();
    let copy_item = context_item.clone();
    let prompt_link = t3_core::prompt_links::user_message_prompt_link(&text);
    let actions = h_flex()
        .justify_end()
        .gap(px(8.))
        .pr(px(4.))
        .max_w(relative(0.8))
        .invisible()
        .group_hover(ROW_GROUP, |style| style.visible())
        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(env.clock.format(&message.created_at, cx)))
        .children(env.revert_counts.get(&message.id).map(|&turn_count| {
            let (message_id, text) = (message.id.clone(), message.text.to_string());
            tooltip_icon_button(
                SharedString::from(format!("rewind:{id}")),
                Icon::Pencil,
                EDIT_FROM_HERE,
                env.rewind_disabled,
                cx.listener(move |view, _, window, cx| view.request_rewind(&message_id, turn_count, &text, window, cx)),
                cx,
            )
        }))
        .children(prompt_link.map(|link| tooltip_icon_button(
            SharedString::from(format!("copy-prompt-link:{id}")), Icon::Link, "Copy prompt link", false,
            cx.listener(move |view, _, _, cx| crate::composer::prompt_links::copy(&view.state, link.clone(), cx)), cx,
        )))
        .child(ghost_icon_button(
            SharedString::from(format!("copy-user:{id}")),
            Icon::Copy,
            cx.listener(move |view, _, _, cx| {
                let environment = view.state.read(cx).server_config.config.as_ref().map(|c|c.environment.environment_id.clone()).unwrap_or_default();
                let records = match copy_item.as_ref().map(|item|&item.kind) {
                    Some(t3_core::turn_item::TurnItemKind::UserMessage { context:Some(context), .. }) => context["records"].as_array().map(Vec::as_slice).unwrap_or_default(),
                    _ => &[],
                };
                let attachments=match copy_item.as_ref().map(|item|&item.kind) {
                    Some(t3_core::turn_item::TurnItemKind::UserMessage {attachments,..})=>attachments.as_slice(),_=>&[],
                };
                let (text,fragment)=t3_core::composer::records::copy_message(&copy_text,records,attachments,&environment,view.state.read(cx).selected_thread.as_deref(),&copy_message_id);
                crate::composer::clipboard::write(text,fragment.unwrap_or_default(),cx);
            }),
            cx,
        ));
    let model_switch = message.meta.model_switch.as_ref().and_then(|value| model_switch(value, cx));
    v_flex()
        .w_full()
        .children(model_switch)
        .child(
            v_flex()
                .group(ROW_GROUP)
                .w_full()
                .items_end()
                .gap(px(4.))
                .when(attribution == Some(Attribution::T3Code), |this| {
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child("Sent by T3 Code"))
                })
                .when_some(message.meta.guest_name.clone(), |this, name| {
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.guest_foreground)).child(name))
                })
                .when(message.is_steer() && !continues_steer, |this| {
                    this.child(
                        h_flex()
                            .mr(px(4.))
                            .gap(px(4.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(Icon::Redo2.render(px(12.), hsla(p.muted_foreground)))
                            .child("Steer"),
                    )
                })
                .children(media_above.map(|media| h_flex().max_w(relative(0.8)).justify_end().child(media)))
                .when(!media_alone, |this| this.child(bubble))
                .child(actions),
        )
        .into_any_element()
}

/// `AssistantForkButton`: on a completed assistant message whose provider can fork.
fn fork_button(message: &ChatMessage, env: &RowEnv, cx: &mut Context<ThreadView>) -> Option<AnyElement> {
    let item = message.item.as_ref().filter(|item| can_fork(item, env.fork_support))?;
    let (run_id, source) = (item.run_id.clone()?, item.thread_id.clone());
    let busy = env.forking.contains(&run_id);
    let id = SharedString::from(format!("fork:{}", message.id));
    Some(tooltip_icon_button(id, Icon::GitFork, FORK_LABEL, busy, cx.listener(move |view, _, _, cx| view.fork_from_run(&source, &run_id, cx)), cx))
}

fn assistant_meta(message: &ChatMessage, show_copy: bool, copy_streaming: bool, always: bool, fork: Option<AnyElement>, clock: &Clock, cx: &App) -> AnyElement {
    let p = palette(cx);
    let status = message.item.as_ref().map(|item| item.status.clone()).filter(|status| status != "completed");
    let copy = (show_copy && !copy_streaming && !message.text.trim().is_empty()).then(|| {
        let text = message.text.to_string();
        ghost_icon_button(SharedString::from(format!("copy-assistant:{}", message.id)), Icon::Copy, move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
        }, cx)
    });
    h_flex()
        .gap(px(8.))
        .text_xs()
        .when(!always, |this| this.invisible().group_hover(ROW_GROUP, |style| style.visible()))
        .children(status.map(|status| {
            div()
                .rounded_full()
                .border_1()
                .border_color(hsla(p.border).opacity(0.7))
                .px(px(6.))
                .py(px(2.))
                .text_size(px(10.))
                .text_color(hsla(p.muted_foreground))
                .child(status)
        }))
        .children(copy)
        .children(fork)
        .when(!message.streaming, |this| this.child(div().text_color(hsla(p.muted_foreground)).child(clock.format(&message.updated_at, cx))))
        .into_any_element()
}

pub fn assistant_row(row: &TimelineRow, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::Message { message, show_assistant_meta, show_copy, copy_streaming, .. } = &row.kind else {
        return div().into_any_element();
    };
    let (id, show_meta, show_copy, copy_streaming) = (row.id.as_str(), *show_assistant_meta, *show_copy, *copy_streaming);
    let text: Arc<str> = if message.text.is_empty() && !message.streaming { Arc::from("(empty response)") } else { message.text.clone() };
    let state = markdown.state(&format!("assistant:{id}"), &text, cx);
    let selection_state = state.entity.clone();
    let message_id = message.id.clone();
    v_flex()
        .group(ROW_GROUP)
        .w_full()
        .min_w_0()
        .px(px(4.))
        .py(px(2.))
        .child(div().id(SharedString::from(format!("assistant-select:{id}")))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(move |view, event: &gpui::MouseUpEvent, window, cx| {
                view.capture_citation_selection(selection_state.clone(), message_id.clone(), text.clone(), event.position, window, cx);
            })).child(prose(&state, cx)))
        .children(env.turn_diffs.get(&message.id).and_then(|summary| super::changed_files::changed_files_card(&message.id, summary, env, cx)))
        .when(show_meta, |this| {
            let fork = fork_button(message, env, cx);
            this.child(div().mt(px(6.)).child(assistant_meta(message, show_copy, copy_streaming, false, fork, &env.clock, cx)))
        })
        .into_any_element()
}

pub fn assistant_meta_row(message: &Arc<ChatMessage>, show_copy: bool, copy_streaming: bool, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let fork = fork_button(message, env, cx);
    div().px(px(4.)).mt(px(2.)).child(assistant_meta(message, show_copy, copy_streaming, true, fork, &env.clock, cx)).into_any_element()
}

fn collapsed_plan_preview(markdown: &str) -> String {
    let mut lines = Vec::new();
    let mut visible = 0;
    let body = strip_displayed_plan_markdown(markdown);
    for line in body.trim_end().lines() {
        let line = line.trim_end();
        if !line.trim().is_empty() {
            if visible >= 10 {
                break;
            }
            visible += 1;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// `ProposedPlanCard`: rounded card, "Plan" badge and title, long plans clipped behind
/// "Expand plan".
pub fn plan_card(id: &str, plan: &Arc<ProposedPlan>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let title = plan_title(&plan.markdown).unwrap_or_else(|| "Proposed plan".into());
    let can_collapse = plan.markdown.len() > 900 || plan.markdown.split('\n').count() > 20;
    let expanded = env.ui.expanded_plans.contains(id);
    let collapsed = can_collapse && !expanded;
    let body: Arc<str> = Arc::from(if collapsed { collapsed_plan_preview(&plan.markdown) } else { strip_displayed_plan_markdown(&plan.markdown) });
    let state = markdown.state(&format!("plan:{id}:{}", u8::from(collapsed)), &body, cx);
    let copy_text = plan.markdown.to_string();
    let card = hsla(p.card).opacity(0.7);
    let key = id.to_owned();
    div()
        .px(px(4.))
        .py(px(2.))
        .child(
            v_flex()
                .w_full()
                .rounded(px(16.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.8))
                .bg(card)
                .p(px(20.))
                .gap(px(16.))
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .rounded(px(4.))
                                        .px(px(3.))
                                        .h(px(18.))
                                        .flex()
                                        .items_center()
                                        .bg(hsla(p.secondary))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.secondary_foreground))
                                        .child("Plan"),
                                )
                                .child(div().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title)),
                        )
                        .child(ghost_icon_button(
                            SharedString::from(format!("copy-plan:{id}")),
                            Icon::Copy,
                            move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone())),
                            cx,
                        )),
                )
                .child(
                    div()
                        .relative()
                        .when(collapsed, |this| this.max_h(px(416.)).overflow_hidden())
                        .child(prose(&state, cx))
                        .when(collapsed, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left_0()
                                    .right_0()
                                    .h(px(96.))
                                    .bg(linear_gradient(180., linear_color_stop(card.opacity(0.), 0.), linear_color_stop(hsla(p.card).opacity(0.95), 1.))),
                            )
                        }),
                )
                .when(can_collapse, |this| {
                    this.child(h_flex().justify_center().child(ghost_text_button(
                        SharedString::from(format!("plan-toggle:{id}")),
                        if expanded { "Collapse plan" } else { "Expand plan" },
                        None,
                        cx.listener(move |view, _, _, cx| view.toggle_plan(&key, cx)),
                        cx,
                    )))
                }),
        )
        .into_any_element()
}
