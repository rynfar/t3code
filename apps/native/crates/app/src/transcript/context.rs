//! Sent-message context chips from `chat/UserMessageContextChips.tsx`.
use crate::{context_chip, thread_view::ThreadView};
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::h_flex;
use serde_json::Value;
use t3_core::composer::records;

pub(super) fn chips(
    message_id: &str,
    text: &str,
    all: &[Value],
    source: Option<&std::sync::Arc<t3_core::turn_item::TurnItem>>,
    state: &gpui::Entity<crate::app_state::AppState>,
    ui: &super::TranscriptUi,
    cx: &mut Context<ThreadView>,
) -> Option<AnyElement> {
    let linked = records::presentation_records(text, all);
    if linked.is_empty() {
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
        let (shell, _) = context_chip::shell(&record, state, false, cx);
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
        let element = div().relative().child(shell).when(open, |s| {
            s.child(crate::composer::controls::floating(
                crate::composer::controls::Float::AboveStart,
                context_chip::details(&record, cx),
            ))
        });
        row = row.child(element);
    }
    Some(row.into_any_element())
}
