//! Sent-message context chips from `chat/UserMessageContextChips.tsx`.
use crate::{context_chip, thread_view::ThreadView};
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::h_flex;
use serde_json::Value;
use t3_core::composer::records;

pub(super) fn chips(
    message: &t3_core::timeline::ChatMessage,
    presentation: &records::SentPresentation,
    citations: &[t3_core::assistant_citations::Citation],
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
    let mut row = h_flex()
        .id(SharedString::from(format!("sent-context:{message_id}")))
        .flex_wrap()
        .gap_x(px(12.))
        .gap_y(px(4.))
        .mb(px(8.))
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && this.ui.open_context.take().is_some() {
                cx.notify();
                cx.stop_propagation();
            }
        }));
    for record in linked {
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
        row = row.child(element);
    }
    let owner = cx.entity().entity_id();
    let citation_ink = crate::composer::context::Ink::Accent(crate::theme::palette(cx).context_chip_citation);
    for (i, citation) in citations.iter().enumerate() {
        let app_state = state.clone();
        let cite = citation.clone();
        let chip = crate::composer::context::chip_shell(
            &format!("sent-citation:{message_id}:{i}"),
            crate::icons::Icon::Quote,
            citation.label(),
            citation_ink,
            true,
            cx,
        )
        .on_click(move |event, window, cx| {
            if event.modifiers().platform {
                super::markdown::links::navigate_to_citation_source(cite.clone(), &app_state, cx);
            } else {
                super::markdown::links::open_sent_citation_popover(cite.clone(), event.position(), app_state.clone(), owner, window, cx);
            }
        });
        row = row.child(div().relative().flex_shrink_0().max_w_full().child(chip));
    }
    Some(row.into_any_element())
}
