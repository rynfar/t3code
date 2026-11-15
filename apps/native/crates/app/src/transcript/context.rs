//! Sent-message context chips and quoted response cards from `chat/UserMessageContextChips.tsx`
//! and `chat/AssistantCitationChip.tsx`.
use crate::{context_chip, theme::{hsla, palette}, thread_view::ThreadView};
use gpui::{
    AnyElement, App, Context, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString,
    Styled, StyledText, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::Value;
use t3_core::{assistant_citations::Citation, composer::records};

fn flat_preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn response_card_box(
    id: impl Into<SharedString>,
    citation: Citation,
    title: Option<String>,
    preview: AnyElement,
    comment: Option<String>,
    state: &gpui::Entity<crate::app_state::AppState>,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let bg = if dark { gpui::black().opacity(0.35) } else { gpui::black().opacity(0.08) };
    let hover_bg = if dark { gpui::black().opacity(0.5) } else { gpui::black().opacity(0.14) };
    let app_state = state.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(4.))
        .child(
            v_flex()
                .id(id.into())
                .w_full()
                .min_w_0()
                .gap(px(2.))
                .rounded(px(8.))
                .px(px(10.))
                .py(px(6.))
                .bg(bg)
                .cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_click(move |_, _, cx| {
                    super::markdown::links::navigate_to_citation_source(citation.clone(), &app_state, cx);
                })
                .when_some(title.filter(|t| !t.trim().is_empty()), |this, title| {
                    this.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground).opacity(0.8))
                            .child(title),
                    )
                })
                .child(preview),
        )
        .when_some(comment.filter(|c| !c.trim().is_empty()), |this, comment| {
            this.child(div().w_full().min_w_0().text_sm().text_color(hsla(p.message_foreground)).child(comment))
        })
        .into_any_element()
}

fn assistant_citation_embed(
    id: String,
    citation: &Citation,
    state: &gpui::Entity<crate::app_state::AppState>,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let whole_response = citation.prefix.is_empty() && citation.suffix.is_empty();
    let quote = flat_preview(&citation.text);
    let preview = if whole_response {
        div().w_full().min_w_0().line_clamp(2).text_sm().text_color(hsla(p.muted_foreground)).child(quote).into_any_element()
    } else {
        let mut text = String::new();
        if !citation.prefix.is_empty() {
            text.push('…');
            text.push_str(&flat_preview(&citation.prefix));
            text.push(' ');
        }
        let quote_start = text.len();
        text.push_str(&quote);
        let quote_end = text.len();
        if !citation.suffix.is_empty() {
            text.push(' ');
            text.push_str(&flat_preview(&citation.suffix));
            text.push('…');
        }
        let highlight = HighlightStyle {
            background_color: Some(hsla(p.foreground).opacity(0.12)),
            color: Some(hsla(p.foreground)),
            ..Default::default()
        };
        div()
            .w_full()
            .min_w_0()
            .line_clamp(2)
            .text_sm()
            .text_color(hsla(p.muted_foreground))
            .child(StyledText::new(text).with_highlights(vec![(quote_start..quote_end, highlight)]))
            .into_any_element()
    };
    response_card_box(id, citation.clone(), None, preview, citation.comment.clone(), state, cx)
}

fn response_reply_card(
    id: String,
    record: &Value,
    state: &gpui::Entity<crate::app_state::AppState>,
    cx: &App,
) -> Option<AnyElement> {
    let citation = records::response_citation(record)?;
    let title = record.get("subagents").and_then(Value::as_array).and_then(|subs| {
        let names: Vec<&str> = subs.iter().filter_map(|s| s.get("title").and_then(Value::as_str)).collect();
        (!names.is_empty()).then(|| names.join(", "))
    });
    let p = palette(cx);
    let preview = div()
        .w_full()
        .min_w_0()
        .line_clamp(2)
        .text_sm()
        .text_color(hsla(p.muted_foreground))
        .child(flat_preview(&citation.text))
        .into_any_element();
    Some(response_card_box(id, citation, title, preview, None, state, cx))
}

pub(super) fn chips(
    message: &t3_core::timeline::ChatMessage,
    presentation: &records::SentPresentation,
    citations: &[Citation],
    all: &[Value],
    excluded_attachment_ids: &[&str],
    env: &super::RowEnv,
    cx: &mut Context<ThreadView>,
) -> Option<AnyElement> {
    let message_id = message.id.as_str();
    let source = message.item.as_ref();
    let (state, ui) = (env.state, env.ui);
    let linked: Vec<_> = presentation
        .records(all)
        .into_iter()
        .filter(|record| {
            let att_id = records::field(record, "attachmentId");
            att_id.is_empty() || !excluded_attachment_ids.contains(&att_id)
        })
        .collect();
    if linked.is_empty() && citations.is_empty() {
        return None;
    }
    let mut cards: Vec<AnyElement> = Vec::new();
    let mut chip_elements: Vec<AnyElement> = Vec::new();
    for (i, record) in linked.into_iter().enumerate() {
        if records::field(&record, "kind") == "response" {
            if let Some(card) = response_reply_card(format!("sent-response:{message_id}:{i}"), &record, state, cx) {
                cards.push(card);
            }
            continue;
        }
        let id = records::field(&record, "contextId").to_owned();
        let open = ui.open_context.as_deref() == Some(id.as_str());
        if records::field(&record, "kind") == "thread" {
            let title = state
                .read(cx)
                .thread_shell(records::field(&record, "threadId"))
                .map(|t| t.title.clone())
                .unwrap_or_default();
            ui.thread_chip_titles
                .borrow_mut()
                .insert(records::field(&record, "threadId").into(), title);
        }
        let attachment = source.and_then(|source| match &source.kind {
            t3_core::turn_item::TurnItemKind::UserMessage { attachments, .. } => attachments.iter().find(|attachment| records::field(attachment, "id") == records::field(&record, "attachmentId")),
            _ => None,
        });
        let shell = match attachment.filter(|_| matches!(records::field(&record, "kind"), "image" | "file")) {
            Some(attachment) => context_chip::attachment_shell(&record, attachment, state, cx),
            None => context_chip::shell(&record, state, false, cx).0,
        };
        let mut shell = match source {
            Some(source) => {
                context_chip::copyable_message(shell, id.clone(), source.clone(), state)
            }
            None => context_chip::copyable_unresolved(shell, &record),
        };
        if context_chip::has_details(&record) {
            shell = shell.on_click(cx.listener(move |this, _, _, cx| {
                this.ui.open_context = if this.ui.open_context.as_deref() == Some(&id) {
                    None
                } else {
                    Some(id.clone())
                };
                cx.notify();
            }));
        }
        let element = div().relative().flex_shrink_0().max_w_full().child(shell).when(open, |s| {
            s.child(crate::composer::controls::floating(
                crate::composer::controls::Float::AboveStart,
                context_chip::details(&record, cx),
            ))
        });
        chip_elements.push(element.into_any_element());
    }
    for (i, citation) in citations.iter().enumerate() {
        cards.push(assistant_citation_embed(format!("sent-citation:{message_id}:{i}"), citation, state, cx));
    }
    if cards.is_empty() && chip_elements.is_empty() {
        return None;
    }
    let mut container = v_flex()
        .id(SharedString::from(format!("sent-context:{message_id}")))
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .mb(px(8.))
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && this.ui.open_context.take().is_some() {
                cx.notify();
                cx.stop_propagation();
            }
        }));
    if !chip_elements.is_empty() {
        container = container.child(
            h_flex()
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .children(chip_elements),
        );
    }
    if !cards.is_empty() {
        container = container.children(cards);
    }
    Some(container.into_any_element())
}
