//! Message rows: the user bubble (`TimelineUserMessageRow.tsx`, §7.5), assistant prose and its
//! meta footer (`TimelineAssistantRows.tsx`, §7.6) and the proposed plan card
//! (`ProposedPlanCard.tsx`).

use std::sync::Arc;

use gpui::{AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, prelude::FluentBuilder, px, relative};
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use t3_core::media::{AssetResource, MediaPlacement};
use t3_core::timeline::{Attribution, ChatMessage, EDIT_FROM_HERE, FORK_LABEL, ProposedPlan, RowKind, TimelineRow, build_collapsed_proposed_plan_preview_markdown, build_proposed_plan_markdown_filename, can_fork, is_workflow_script_message, normalize_plan_markdown_for_export, partition_user_attachments, proposed_plan_title, render_codex_directives_for_copy, resolve_model_switch_change, resolve_user_message_presentation, should_collapse_user_message, strip_displayed_plan_markdown, user_message_file_label};

use super::markdown::{MarkdownStore, markdown_view, prose};
use super::parts::{CopyButton, DividerTone, ROW_GROUP, Clock, focusable, ghost_button, ghost_icon_button, ghost_text_button, mono_frame, outline_button, system_divider, tooltip_icon_button};
use super::RowEnv;
use crate::app_state::AppState;
use crate::composer::controls::{Float, MenuItem, floating, menu_surface};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;
use crate::toast::{Toast, ToastKind};

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

/// `ModelSwitchRow`: what changed between the previous turn's model and this one.
fn model_switch(value: &Value, providers: &[t3_core::providers::ProviderEntry], cx: &App) -> Option<AnyElement> {
    let display = resolve_model_switch_change(value, providers)?;
    let label = format!("{} → {}", display.from_summary, display.to_summary);
    Some(system_divider(label.into(), None, None, DividerTone::Neutral, cx))
}

fn first_line_preview(text: &str) -> String {
    let trimmed = text.trim();
    trimmed.lines().next().unwrap_or_default().trim().to_owned()
}

#[derive(IntoElement)]
struct WorkflowOpeningCard {
    id: SharedString,
    script: String,
    surface: gpui::Hsla,
    actions: AnyElement,
}

impl RenderOnce for WorkflowOpeningCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let expanded_state = window.use_keyed_state(SharedString::from(format!("workflow-open:{}", self.id)), cx, |_, _| false);
        let expanded = *expanded_state.read(cx);
        let toggle_state = expanded_state.clone();
        let chevron = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
        v_flex()
            .group(ROW_GROUP)
            .w_full()
            .items_end()
            .gap(px(4.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(relative(0.8))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(self.surface)
                    .child(
                        focusable(
                            h_flex()
                                .id(SharedString::from(format!("workflow-toggle:{}", self.id)))
                                .w_full()
                                .justify_between()
                                .gap(px(8.))
                                .px(px(12.))
                                .py(px(8.))
                                .cursor_pointer()
                                .hover(|style| style.bg(hsla(p.accent).opacity(0.35)))
                                .child(
                                    h_flex()
                                        .min_w_0()
                                        .gap(px(6.))
                                        .child(Icon::CodeXml.render(px(14.), hsla(p.muted_foreground)))
                                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Workflow script")),
                                )
                                .child(chevron.render(px(12.), hsla(p.muted_foreground)))
                                .on_click(move |_, _, cx| {
                                    toggle_state.update(cx, |open, cx| {
                                        *open = !*open;
                                        cx.notify();
                                    });
                                }),
                            cx,
                        ),
                    )
                    .when(expanded, |this| {
                        this.child(
                            div()
                                .px(px(12.))
                                .pb(px(10.))
                                .pt(px(2.))
                                .border_t_1()
                                .border_color(hsla(p.border).opacity(0.4))
                                .child(mono_frame(cx).child(self.script)),
                        )
                    }),
            )
            .child(self.actions)
    }
}

#[derive(IntoElement)]
struct CollapsibleAttributionCard {
    id: SharedString,
    is_automation: bool,
    label: String,
    preview: String,
    surface: gpui::Hsla,
    source_thread_id: Option<String>,
    state: Entity<AppState>,
    media_above: Option<AnyElement>,
    bubble: Option<AnyElement>,
    actions: AnyElement,
}

impl RenderOnce for CollapsibleAttributionCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let expanded_state = window.use_keyed_state(SharedString::from(format!("attr-open:{}", self.id)), cx, |_, _| false);
        let expanded = *expanded_state.read(cx);
        let toggle_state = expanded_state.clone();
        let chevron = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
        let icon = if self.is_automation { Icon::CalendarClock } else { Icon::MessagesSquare };
        let app_state = self.state.clone();
        let source_thread_id = self.source_thread_id.clone();
        let open_action = if self.is_automation {
            Some(ghost_text_button(
                SharedString::from(format!("open-automation:{}", self.id)),
                "Open automation",
                Some(Icon::ExternalLink),
                |_, _, cx| {
                    cx.stop_propagation();
                    crate::settings::open(t3_core::settings::search::Section::ScheduledTasks, cx);
                },
                cx,
            ))
        } else {
            source_thread_id.map(|thread_id| {
                ghost_text_button(
                    SharedString::from(format!("open-agent-thread:{}", self.id)),
                    "Open thread",
                    Some(Icon::ExternalLink),
                    move |_, _, cx| {
                        cx.stop_propagation();
                        app_state.update(cx, |s, cx| s.open_related_thread(&thread_id, cx));
                    },
                    cx,
                )
            })
        };
        v_flex()
            .group(ROW_GROUP)
            .w_full()
            .items_end()
            .gap(px(4.))
            .child(
                h_flex()
                    .max_w(relative(0.8))
                    .justify_between()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(self.surface)
                    .child(
                        focusable(
                            h_flex()
                                .id(SharedString::from(format!("attr-toggle:{}", self.id)))
                                .min_w_0()
                                .flex_1()
                                .gap(px(6.))
                                .cursor_pointer()
                                .child(icon.render(px(14.), hsla(p.muted_foreground)))
                                .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.label))
                                .when(!expanded && !self.preview.is_empty(), |this| {
                                    this.child(div().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(self.preview))
                                })
                                .child(chevron.render(px(12.), hsla(p.muted_foreground)))
                                .on_click(move |_, _, cx| {
                                    toggle_state.update(cx, |open, cx| {
                                        *open = !*open;
                                        cx.notify();
                                    });
                                }),
                            cx,
                        ),
                    )
                    .children(open_action),
            )
            .when(expanded, |this| {
                this.children(self.media_above).children(self.bubble)
            })
            .child(self.actions)
    }
}

fn render_user_file_cards(id: &str, cards: &[t3_core::timeline::UserFileCard], cx: &App) -> Option<AnyElement> {
    if cards.is_empty() {
        return None;
    }
    let p = palette(cx);
    Some(
        v_flex()
            .gap(px(4.))
            .mb(px(6.))
            .children(cards.iter().enumerate().map(|(index, card)| {
                let attachment_id = card
                    .attachment
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty());
                let row = h_flex()
                    .id(SharedString::from(format!("user-file:{id}:{index}")))
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .bg(hsla(p.secondary).opacity(0.35))
                    .text_size(px(13.))
                    .child((if card.pasted_text { Icon::FileText } else { Icon::File }).render(px(14.), hsla(p.muted_foreground)))
                    .child(div().min_w_0().truncate().text_color(hsla(p.foreground)).child(card.label.clone()))
                    .when_some(card.size_bytes, |row, bytes| {
                        row.child(div().flex_shrink_0().text_xs().text_color(hsla(p.muted_foreground)).child(t3_core::composer::attachments::format_size(bytes)))
                    });
                if card.downloadable
                    && let Some(attachment_id) = attachment_id
                {
                    focusable(
                        row.cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent).opacity(0.55)))
                            .on_click(move |_, _, cx| crate::media::open_asset(&AssetResource::Attachment { attachment_id: attachment_id.clone() }, cx)),
                        cx,
                    )
                    .into_any_element()
                } else {
                    row.into_any_element()
                }
            }))
            .into_any_element(),
    )
}

fn render_user_audio_cards(id: &str, audios: &[Value], cx: &App) -> Option<AnyElement> {
    if audios.is_empty() {
        return None;
    }
    let p = palette(cx);
    Some(
        v_flex()
            .gap(px(4.))
            .mb(px(6.))
            .children(audios.iter().enumerate().map(|(index, audio)| {
                let label = user_message_file_label(audio);
                let attachment_id = audio.get("id").and_then(Value::as_str).map(str::to_owned).filter(|s| !s.is_empty());
                let row = h_flex()
                    .id(SharedString::from(format!("user-audio:{id}:{index}")))
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .bg(hsla(p.secondary).opacity(0.35))
                    .text_size(px(13.))
                    .child(Icon::FileText.render(px(14.), hsla(p.muted_foreground)))
                    .child(div().min_w_0().truncate().text_color(hsla(p.foreground)).child(label));
                if let Some(attachment_id) = attachment_id {
                    focusable(
                        row.cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent).opacity(0.55)))
                            .on_click(move |_, _, cx| crate::media::open_asset(&AssetResource::Attachment { attachment_id: attachment_id.clone() }, cx)),
                        cx,
                    )
                    .into_any_element()
                } else {
                    row.into_any_element()
                }
            }))
            .into_any_element(),
    )
}

pub fn user_row(row: &TimelineRow, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::Message { message, grouped_with_next, grouped_with_previous, continues_steer, .. } = &row.kind else {
        return div().into_any_element();
    };
    let (id, grouped_with_next, grouped_with_previous, continues_steer) = (row.id.as_str(), *grouped_with_next, *grouped_with_previous, *continues_steer);
    let p = palette(cx);
    let resolved = resolve_user_message_presentation(message);
    let attribution = resolved.attribution;
    let raw_text: Arc<str> = match &message.meta.guest_text {
        Some(text) => Arc::from(text.as_str()),
        None => Arc::from(resolved.text.as_str()),
    };
    let (fresh_context, workflow_script, item_status, records, attachments) = match message.item.as_ref().map(|item| (&item.kind, item.status.as_str())) {
        Some((t3_core::turn_item::TurnItemKind::UserMessage { context, attachments, workflow_script, fresh_context, .. }, status)) => (
            *fresh_context,
            *workflow_script,
            Some(status),
            context.as_ref().and_then(|c| c["records"].as_array()).map(Vec::as_slice).unwrap_or_default(),
            attachments.as_slice(),
        ),
        _ => (None, None, None, &[][..], &[][..]),
    };
    let context_item = message.item.clone();
    let presentation = markdown.user_context(id, &raw_text);
    let context_chips = super::context::chips(id, &presentation, records, context_item.as_ref(), env.state, env.ui, cx);
    let display_text = presentation.display_text.as_ref();
    let collapsible = should_collapse_user_message(display_text);
    let expanded = env.ui.expanded_messages.contains(id);
    let collapsed = collapsible && !expanded;
    let surface = hsla(if attribution == Some(Attribution::Guest) { p.guest_surface } else { p.message_surface });
    let copy_text = message.meta.guest_text.clone().unwrap_or_else(|| message.text.to_string());
    let copy_message_id = message.id.clone();
    let copy_item = context_item.clone();
    let copy_state = env.state.clone();
    let copy_thread_id = env.thread_id.map(str::to_owned);
    let prompt_link = t3_core::prompt_links::user_message_prompt_link(&raw_text);
    let sending = matches!(item_status, Some("pending" | "waiting"));
    let error_status = item_status.filter(|s| !s.is_empty() && *s != "completed" && !matches!(*s, "pending" | "waiting")).map(str::to_owned);
    let actions = h_flex()
        .justify_end()
        .gap(px(8.))
        .pr(px(4.))
        .max_w(relative(0.8))
        .when(!sending && error_status.is_none(), |this| this.invisible().group_hover(ROW_GROUP, |style| style.visible()))
        .when(sending, |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Sending...")))
        .when_some(error_status, |this, status| {
            this.child(
                div()
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(p.destructive).opacity(0.4))
                    .bg(hsla(p.destructive).opacity(0.1))
                    .px(px(6.))
                    .py(px(1.))
                    .text_size(px(10.))
                    .text_color(hsla(p.destructive))
                    .child(status),
            )
        })
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
        .child(CopyButton::new(SharedString::from(format!("copy-user:{id}")), "Copy message", move |_, cx| {
            let environment = copy_state.read(cx).server_config.config.as_ref().map(|c| c.environment.environment_id.clone()).unwrap_or_default();
            let records = match copy_item.as_ref().map(|item| &item.kind) {
                Some(t3_core::turn_item::TurnItemKind::UserMessage { context: Some(context), .. }) => context["records"].as_array().map(Vec::as_slice).unwrap_or_default(),
                _ => &[],
            };
            let attachments = match copy_item.as_ref().map(|item| &item.kind) {
                Some(t3_core::turn_item::TurnItemKind::UserMessage { attachments, .. }) => attachments.as_slice(),
                _ => &[],
            };
            let (text, fragment) = t3_core::composer::records::copy_message(&copy_text, records, attachments, &environment, copy_thread_id.as_deref(), &copy_message_id);
            crate::composer::clipboard::write(text, fragment.unwrap_or_default(), cx);
        }))
        .into_any_element();

    let providers = env
        .state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .map(|config| t3_core::providers::entries(config, None))
        .unwrap_or_default();
    let fresh_divider = (fresh_context == Some(true)).then(|| system_divider("New session".into(), None, Some(Icon::RotateCcw), DividerTone::Neutral, cx));
    let model_switch = message.meta.model_switch.as_ref().and_then(|value| model_switch(value, &providers, cx));

    if is_workflow_script_message(message.role, &message.text, workflow_script.or(message.meta.workflow_script)) {
        return v_flex()
            .w_full()
            .children(fresh_divider)
            .children(model_switch)
            .child(WorkflowOpeningCard {
                id: SharedString::from(id.to_owned()),
                script: message.text.to_string(),
                surface,
                actions,
            })
            .into_any_element();
    }

    let body = if display_text.trim().is_empty() {
        None
    } else {
        let source: Arc<str> = Arc::from(with_line_breaks(display_text));
        let state = markdown.state(&format!("user:{id}"), &source, cx);
        Some(markdown_view(&state, cx))
    };
    let partitioned = partition_user_attachments(attachments, display_text);
    let images = t3_core::media::image_attachments(attachments);
    let inline_media = matches!(attribution, Some(Attribution::Guest | Attribution::Agent));
    let placement = t3_core::media::media_placement(images.len(), body.is_none(), partitioned.files.len() + partitioned.audios.len(), inline_media);
    let media = placement.map(|placement| super::media::user_media(id, &images, placement, grouped_with_previous, grouped_with_next, cx));
    let (media_above, media_in_bubble) = match placement {
        Some(MediaPlacement::InBubble) => (None, media),
        _ => (media, None),
    };
    let small = |radius: bool| if radius { px(6.) } else { px(16.) };
    let media_alone = placement == Some(MediaPlacement::Alone);
    let file_cards = render_user_file_cards(id, &partitioned.files, cx);
    let audio_cards = render_user_audio_cards(id, &partitioned.audios, cx);
    let standalone_cards = (partitioned.file_cards_only || partitioned.audio_only)
        .then(|| v_flex().max_w(relative(0.8)).items_end().children(file_cards.as_ref().and_then(|_| render_user_file_cards(id, &partitioned.files, cx))).children(audio_cards.as_ref().and_then(|_| render_user_audio_cards(id, &partitioned.audios, cx))).into_any_element());
    let bubble = (!media_alone && !partitioned.file_cards_only && !partitioned.audio_only).then(|| {
        v_flex()
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
            .children(file_cards)
            .children(audio_cards)
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
            })
            .into_any_element()
    });

    if matches!(attribution, Some(Attribution::Automation | Attribution::Agent)) {
        let is_automation = attribution == Some(Attribution::Automation);
        let label = if is_automation {
            "Automation".to_owned()
        } else {
            message.meta.guest_name.clone().unwrap_or_else(|| "Agent".to_owned())
        };
        let preview = first_line_preview(display_text);
        return v_flex()
            .w_full()
            .children(fresh_divider)
            .children(model_switch)
            .child(CollapsibleAttributionCard {
                id: SharedString::from(id.to_owned()),
                is_automation,
                label,
                preview,
                surface,
                source_thread_id: message.meta.sender_thread_id.clone(),
                state: env.state.clone(),
                media_above: media_above.map(|media| h_flex().max_w(relative(0.8)).justify_end().child(media).into_any_element()),
                bubble: standalone_cards.or(bubble),
                actions,
            })
            .into_any_element();
    }

    v_flex()
        .w_full()
        .children(fresh_divider)
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
                .children(standalone_cards)
                .children(bubble)
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
        let text = render_codex_directives_for_copy(&message.text);
        CopyButton::new(SharedString::from(format!("copy-assistant:{}", message.id)), "Copy message", move |_, cx| {
            crate::composer::clipboard::write(text.clone(), String::new(), cx);
        })
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
    let selection_state = state.clone();
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

#[derive(Default)]
struct PlanCardUi {
    menu_open: bool,
    save_open: bool,
    saving: bool,
    saved_path: Option<String>,
    save_error: Option<String>,
    path_input: Option<Entity<InputState>>,
}

type ToggleHandler = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
struct PlanCard {
    id: SharedString,
    plan: Arc<ProposedPlan>,
    expanded: bool,
    prose_state: super::markdown::MarkdownState,
    can_collapse: bool,
    on_toggle_expand: ToggleHandler,
    workspace_root: Option<String>,
    state: Entity<AppState>,
}

impl RenderOnce for PlanCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let ui = window.use_keyed_state(SharedString::from(format!("plan-ui:{}", self.id)), cx, |_, _| PlanCardUi::default());
        let (menu_open, save_open, saving, saved_path, save_error, path_input) = {
            let u = ui.read(cx);
            (u.menu_open, u.save_open, u.saving, u.saved_path.clone(), u.save_error.clone(), u.path_input.clone())
        };
        let title = proposed_plan_title(&self.plan.markdown).unwrap_or_else(|| "Proposed plan".into());
        let export_markdown = normalize_plan_markdown_for_export(&self.plan.markdown);
        let default_filename = build_proposed_plan_markdown_filename(&self.plan.markdown);
        let card = hsla(p.card).opacity(0.7);
        let collapsed = self.can_collapse && !self.expanded;
        let can_write_workspace = self
            .state
            .read(cx)
            .client_handle()
            .and_then(|h| h.session_state().borrow().clone())
            .is_none_or(|session| session.grants(t3_core::auth::FILESYSTEM_WRITE_SCOPE));

        let ui_menu_toggle = ui.clone();
        let ui_menu_dismiss = ui.clone();
        let ui_copy_menu = ui.clone();
        let ui_download = ui.clone();
        let ui_save_open = ui.clone();
        let ui_save_cancel = ui.clone();
        let ui_save_submit = ui.clone();

        let copy_for_btn = export_markdown.clone();
        let copy_for_menu = export_markdown.clone();
        let download_markdown = export_markdown.clone();
        let download_filename = default_filename.clone();
        let download_dir = self.workspace_root.clone();
        let download_state = self.state.clone();
        let save_default_filename = default_filename.clone();
        let save_markdown = export_markdown;
        let save_cwd = self.workspace_root.clone();
        let save_state = self.state.clone();
        let on_toggle_expand = self.on_toggle_expand;
        let id = self.id;

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
                                            .px(px(4.))
                                            .h(px(18.))
                                            .flex()
                                            .items_center()
                                            .bg(hsla(p.secondary))
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.secondary_foreground))
                                            .child("Plan"),
                                    )
                                    .when(self.plan.streaming, |this| {
                                        this.child(
                                            div()
                                                .rounded(px(4.))
                                                .px(px(5.))
                                                .h(px(18.))
                                                .flex()
                                                .items_center()
                                                .bg(hsla(p.accent).opacity(0.7))
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .child("Streaming..."),
                                        )
                                    })
                                    .child(div().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title)),
                            )
                            .child(
                                h_flex()
                                    .relative()
                                    .flex_none()
                                    .gap(px(4.))
                                    .child(CopyButton::new(SharedString::from(format!("copy-plan:{id}")), "Copy plan", move |_, cx| {
                                        crate::composer::clipboard::write(copy_for_btn.clone(), String::new(), cx);
                                    }))
                                    .child(ghost_icon_button(
                                        SharedString::from(format!("plan-more:{id}")),
                                        Icon::Ellipsis,
                                        move |_, _, cx| {
                                            ui_menu_toggle.update(cx, |u, cx| {
                                                u.menu_open = !u.menu_open;
                                                cx.notify();
                                            });
                                        },
                                        cx,
                                    ))
                                    .when(menu_open, |this| {
                                        let id_str = id.clone();
                                        let popup = menu_surface(cx)
                                            .w(px(210.))
                                            .on_mouse_down_out(move |_, _, cx| {
                                                ui_menu_dismiss.update(cx, |u, cx| {
                                                    u.menu_open = false;
                                                    cx.notify();
                                                });
                                            })
                                            .child(
                                                MenuItem::new(SharedString::from(format!("plan-menu-copy:{id_str}")), "Copy to clipboard")
                                                    .icon(Icon::Copy, hsla(p.muted_foreground))
                                                    .on_click(move |_, _, cx| {
                                                        crate::composer::clipboard::write(copy_for_menu.clone(), String::new(), cx);
                                                        ui_copy_menu.update(cx, |u, cx| {
                                                            u.menu_open = false;
                                                            cx.notify();
                                                        });
                                                    }),
                                            )
                                            .child(
                                                MenuItem::new(SharedString::from(format!("plan-menu-download:{id_str}")), "Download as markdown")
                                                    .icon(Icon::Download, hsla(p.muted_foreground))
                                                    .on_click(move |_, _, cx| {
                                                        ui_download.update(cx, |u, cx| {
                                                            u.menu_open = false;
                                                            cx.notify();
                                                        });
                                                        let dir = download_dir.as_deref().map(std::path::Path::new).unwrap_or_else(|| std::path::Path::new("/"));
                                                        let rx = cx.prompt_for_new_path(dir, Some(&download_filename));
                                                        let content = download_markdown.clone();
                                                        let state = download_state.clone();
                                                        cx.spawn(async move |cx| {
                                                            let Ok(Ok(Some(path))) = rx.await else {
                                                                return;
                                                            };
                                                            let result = cx.background_executor().spawn(async move { std::fs::write(&path, content) }).await;
                                                            if let Err(err) = result {
                                                                state.update(cx, |s, cx| {
                                                                    s.show_toast(Toast::new(ToastKind::Error, "Failed to download plan").description(err.to_string()), cx);
                                                                });
                                                            }
                                                        })
                                                        .detach();
                                                    }),
                                            )
                                            .when(self.workspace_root.is_some(), |menu| {
                                                menu.child(
                                                    MenuItem::new(SharedString::from(format!("plan-menu-save:{id_str}")), "Save to workspace")
                                                        .icon(Icon::FolderPlus, hsla(p.muted_foreground))
                                                        .on_click(move |_, window, cx| {
                                                            let default_name = save_default_filename.clone();
                                                            ui_save_open.update(cx, |u, cx| {
                                                                u.menu_open = false;
                                                                u.save_open = true;
                                                                u.save_error = None;
                                                                if u.path_input.is_none() {
                                                                    u.path_input = Some(cx.new(|cx| InputState::new(window, cx).default_value(default_name)));
                                                                }
                                                                cx.notify();
                                                            });
                                                        }),
                                                )
                                            });
                                        this.child(floating(Float::BelowEnd, div().pt(px(4.)).child(popup)))
                                    }),
                            ),
                    )
                    .when(save_open, |this| {
                        let input = path_input.clone();
                        this.child(
                            v_flex()
                                .gap(px(8.))
                                .p(px(12.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(hsla(p.border).opacity(0.7))
                                .bg(hsla(p.secondary).opacity(0.35))
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Save plan to workspace"))
                                .children(input.map(|input| {
                                    let save_input = input.clone();
                                    h_flex()
                                        .gap(px(8.))
                                        .child(div().flex_1().min_w_0().child(Input::new(&input).disabled(saving)))
                                        .child(outline_button(
                                            SharedString::from(format!("plan-save-confirm:{id}")),
                                            if saving { "Saving..." } else { "Save" },
                                            move |_, _, cx| {
                                                if saving || !can_write_workspace {
                                                    return;
                                                }
                                                let Some(cwd) = save_cwd.clone() else { return };
                                                let rel = save_input.read(cx).value().trim().to_owned();
                                                if rel.is_empty() {
                                                    ui_save_submit.update(cx, |u, cx| {
                                                        u.save_error = Some("Enter a relative path in the workspace.".to_owned());
                                                        cx.notify();
                                                    });
                                                    return;
                                                }
                                                ui_save_submit.update(cx, |u, cx| {
                                                    u.saving = true;
                                                    u.save_error = None;
                                                    u.saved_path = None;
                                                    cx.notify();
                                                });
                                                let task = save_state.update(cx, |s, _| {
                                                    s.rpc(t3_core::commands::method::PROJECTS_WRITE_FILE, t3_core::commands::project_write_file(&cwd, &rel, &save_markdown))
                                                });
                                                let weak = ui_save_submit.downgrade();
                                                cx.spawn(async move |cx| {
                                                    let result = task.await;
                                                    let _ = weak.update(cx, |u, cx| {
                                                        u.saving = false;
                                                        match result {
                                                            Ok(val) => {
                                                                let written = val.get("relativePath").and_then(Value::as_str).unwrap_or(&rel).to_owned();
                                                                u.saved_path = Some(written);
                                                                u.save_open = false;
                                                            }
                                                            Err(err) => u.save_error = Some(err.to_string()),
                                                        }
                                                        cx.notify();
                                                    });
                                                })
                                                .detach();
                                            },
                                            cx,
                                        ))
                                        .child(ghost_button(
                                            SharedString::from(format!("plan-save-cancel:{id}")),
                                            "Cancel",
                                            saving,
                                            move |_, _, cx| {
                                                ui_save_cancel.update(cx, |u, cx| {
                                                    u.save_open = false;
                                                    u.save_error = None;
                                                    cx.notify();
                                                });
                                            },
                                            cx,
                                        ))
                                }))
                                .when(!can_write_workspace, |this| {
                                    this.child(div().text_xs().text_color(hsla(p.destructive)).child("Missing filesystem:write permission on this connection."))
                                })
                                .when_some(save_error, |this, err| this.child(div().text_xs().text_color(hsla(p.destructive)).child(err))),
                        )
                    })
                    .when_some(saved_path, |this, path| {
                        this.child(
                            h_flex()
                                .gap(px(6.))
                                .text_xs()
                                .text_color(hsla(p.primary))
                                .child(Icon::Check.render(px(12.), hsla(p.primary)))
                                .child(format!("Saved to {path}")),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .when(collapsed, |this| this.max_h(px(416.)).overflow_hidden())
                            .child(prose(&self.prose_state, cx))
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
                    .when(self.can_collapse, |this| {
                        this.child(h_flex().justify_center().child(ghost_text_button(
                            SharedString::from(format!("plan-toggle:{id}")),
                            if self.expanded { "Collapse plan" } else { "Expand plan" },
                            None,
                            move |_, window, cx| on_toggle_expand(window, cx),
                            cx,
                        )))
                    }),
            )
    }
}

/// `ProposedPlanCard`: rounded card, "Plan" badge and title, long plans clipped behind
/// "Expand plan".
pub fn plan_card(id: &str, plan: &Arc<ProposedPlan>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let can_collapse = plan.markdown.len() > 900 || plan.markdown.split('\n').count() > 20;
    let expanded = env.ui.expanded_plans.contains(id);
    let collapsed = can_collapse && !expanded;
    let body: Arc<str> = Arc::from(if collapsed {
        build_collapsed_proposed_plan_preview_markdown(&plan.markdown, 10).0
    } else {
        strip_displayed_plan_markdown(&plan.markdown)
    });
    let prose_state = markdown.state(&format!("plan:{id}:{}", u8::from(collapsed)), &body, cx);
    let key = id.to_owned();
    let view = cx.entity();
    PlanCard {
        id: SharedString::from(id.to_owned()),
        plan: plan.clone(),
        expanded,
        prose_state,
        can_collapse,
        on_toggle_expand: std::rc::Rc::new(move |_, cx| {
            view.update(cx, |view, cx| view.toggle_plan(&key, cx));
        }),
        workspace_root: env.workspace_root.map(str::to_owned),
        state: env.state.clone(),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_line_breaks_adds_hard_breaks_outside_fences() {
        let input = "line one\nline two\n```rs\nlet a = 1;\nlet b = 2;\n```\nline three";
        assert_eq!(
            with_line_breaks(input),
            "line one  \nline two  \n```rs\nlet a = 1;\nlet b = 2;\n```  \nline three"
        );
    }

    #[test]
    fn first_line_preview_trims_leading_blank_lines() {
        assert_eq!(first_line_preview("\n\n  First line  \nSecond line"), "First line");
    }
}
