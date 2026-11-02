//! Rendering for one turn item. First cut: every kind has a readable row; grouping of tool
//! calls into work groups (the web's `MessagesTimeline.logic.ts`) is tracked in the ledger.

use gpui::{AnyElement, App, Entity, IntoElement, ParentElement, SharedString, Styled, div, px};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use t3_core::turn_item::{TurnItem, TurnItemKind};

pub fn render_item(item: &TurnItem, markdown: Option<Entity<TextViewState>>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    match &item.kind {
        TurnItemKind::UserMessage { text, .. } => h_flex()
            .justify_end()
            .child(
                div()
                    .max_w(px(620.))
                    .px_3p5()
                    .py_2()
                    .rounded_xl()
                    .bg(theme.secondary)
                    .text_color(theme.secondary_foreground)
                    .text_sm()
                    .child(SharedString::from(text.clone())),
            )
            .into_any_element(),
        TurnItemKind::AssistantMessage { .. } | TurnItemKind::ProposedPlan { .. } => match markdown {
            Some(state) => div().text_sm().child(TextView::new(&state)).into_any_element(),
            None => div().into_any_element(),
        },
        TurnItemKind::Reasoning { text, streaming } => {
            let first = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("Thinking");
            work_line(IconName::Bot, if *streaming { "Thinking" } else { "Thought" }, Some(first), false, cx)
        }
        TurnItemKind::CommandExecution { input, exit_code, output_indicates_failure, .. } => {
            let failed = exit_code.is_some_and(|code| code != 0) || output_indicates_failure == &Some(true);
            work_line(IconName::SquareTerminal, "Ran", Some(input.lines().next().unwrap_or_default()), failed, cx)
        }
        TurnItemKind::FileChange { file_name, additions, deletions, .. } => {
            let stats = match (additions, deletions) {
                (Some(a), Some(d)) => format!("{file_name}  +{a} −{d}"),
                _ => file_name.clone(),
            };
            work_line(IconName::File, "Edited", Some(&stats), item.status == "failed", cx)
        }
        TurnItemKind::FileSearch { pattern, .. } => {
            work_line(IconName::Search, "Searched", pattern.as_deref(), false, cx)
        }
        TurnItemKind::WebSearch { patterns, .. } => {
            let query = patterns.as_ref().and_then(|p| p.first()).map(String::as_str);
            work_line(IconName::Globe, "Searched the web", query, false, cx)
        }
        TurnItemKind::DynamicTool { tool_name, .. } => {
            work_line(IconName::Settings2, "Called", tool_name.as_deref().or(item.title.as_deref()), item.status == "failed", cx)
        }
        TurnItemKind::Subagent { prompt, .. } => {
            work_line(IconName::Bot, "Subagent", prompt.lines().next(), item.status == "failed", cx)
        }
        TurnItemKind::TodoList { steps, .. } => v_flex()
            .gap_1()
            .py_1()
            .children(steps.iter().map(|step| {
                let done = step.status == "completed";
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(if done { theme.muted_foreground } else { theme.foreground })
                    .child(Icon::new(if done { IconName::CircleCheck } else { IconName::Minus }).xsmall())
                    .child(SharedString::from(step.text.clone()))
            }))
            .into_any_element(),
        TurnItemKind::ApprovalRequest { prompt, reason, request_kind, .. } => callout(
            IconName::TriangleAlert,
            "Approval requested",
            prompt.as_deref().or(reason.as_deref()).unwrap_or(request_kind),
            theme.warning,
            cx,
        ),
        TurnItemKind::UserInputRequest { questions, .. } => {
            let first = questions.first().and_then(|q| q.get("question")).and_then(|q| q.as_str()).unwrap_or("The agent has a question.");
            callout(IconName::Info, "Question", first, theme.info, cx)
        }
        TurnItemKind::Error { failure } => callout(IconName::CircleX, "Error", &failure.message, theme.danger, cx),
        TurnItemKind::Checkpoint { files, .. } => {
            let (adds, dels) = files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            let label = format!("{} file{} changed  +{adds} −{dels}", files.len(), if files.len() == 1 { "" } else { "s" });
            notice(&label, cx)
        }
        TurnItemKind::Compaction { .. } => notice("Context compacted", cx),
        TurnItemKind::SystemNotice { message }
        | TurnItemKind::RunInterruptRequest { message }
        | TurnItemKind::RunInterruptResult { message } => notice(message, cx),
        TurnItemKind::Notification { summary, .. } => notice(summary, cx),
        TurnItemKind::Handoff { to_model, .. } => {
            notice(&format!("Handed off{}", to_model.as_ref().map(|m| format!(" to {m}")).unwrap_or_default()), cx)
        }
        TurnItemKind::Fork { .. } => notice("Forked", cx),
        TurnItemKind::ThreadCreated { target_model, .. } => notice(&format!("Started a thread with {target_model}"), cx),
        TurnItemKind::SecretRequest { label, .. } => callout(IconName::Info, "Secret requested", label, theme.info, cx),
        TurnItemKind::LinkRequest { reason, .. } => callout(IconName::Network, "Link requested", reason, theme.info, cx),
        TurnItemKind::Unknown => div().into_any_element(),
    }
}

fn work_line(icon: IconName, verb: &str, detail: Option<&str>, failed: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .h(px(24.))
        .text_sm()
        .text_color(if failed { theme.danger } else { theme.muted_foreground })
        .child(Icon::new(icon).xsmall())
        .child(div().flex_none().child(SharedString::from(verb.to_owned())))
        .children(detail.map(|detail| {
            div().min_w_0().truncate().font_family(theme.mono_font_family.clone()).text_xs().child(SharedString::from(detail.to_owned()))
        }))
        .into_any_element()
}

fn callout(icon: IconName, title: &str, body: &str, accent: gpui::Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(accent.opacity(0.4))
        .bg(accent.opacity(0.06))
        .child(
            h_flex()
                .gap_2()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(accent)
                .child(Icon::new(icon).small())
                .child(SharedString::from(title.to_owned())),
        )
        .child(div().text_sm().text_color(theme.foreground).child(SharedString::from(body.to_owned())))
        .into_any_element()
}

fn notice(text: &str, cx: &App) -> AnyElement {
    div()
        .w_full()
        .text_center()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .py_1()
        .child(SharedString::from(text.to_owned()))
        .into_any_element()
}
