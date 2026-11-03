//! Shared context presentation from `ThreadContextChip.tsx`, `EnvironmentContextChip.tsx`,
//! `contextChipParts.tsx` and `chat/UserMessageContextChips.tsx`.
use crate::{
    app_state::AppState,
    composer::context::{Ink, chip_shell, chip_shell_with_icon},
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{
    App, Entity, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    StyledImage, div, img, prelude::*, px,
};
use gpui_component::{ActiveTheme, tooltip::Tooltip, v_flex};
use serde_json::Value;
use t3_core::composer::records::{self as rules, field};

fn accent(kind: &str, cx: &App) -> u32 {
    let p = palette(cx);
    match kind {
        "image" => p.context_chip_image,
        "video" => p.context_chip_video,
        "file" => p.context_chip_file,
        "mention" => p.context_chip_mention,
        "terminal" => p.context_chip_terminal,
        "element" => p.context_chip_element,
        "preview-annotation" => p.context_chip_preview_annotation,
        "review-comment" => p.context_chip_review_comment,
        "pull-request" => p.context_chip_pull_request,
        "pr-open" => p.context_chip_pr_open,
        "pr-draft" => p.context_chip_pr_draft,
        "pr-merged" => p.context_chip_pr_merged,
        "pr-closed" => p.context_chip_pr_closed,
        "skill" => p.context_chip_skill,
        "thread" => p.context_chip_thread,
        "environment" => p.context_chip_environment,
        "citation" => p.context_chip_citation,
        "doc-pdf" => p.context_chip_doc_pdf,
        "doc-word" => p.context_chip_doc_word,
        "doc-spreadsheet" => p.context_chip_doc_spreadsheet,
        "doc-slides" => p.context_chip_doc_slides,
        "doc-epub" => p.context_chip_doc_epub,
        "invalid" => p.context_chip_invalid,
        _ => p.foreground,
    }
}

pub(crate) fn has_details(record: &Value) -> bool {
    matches!(
        field(record, "kind"),
        "terminal" | "element" | "preview-annotation" | "review-comment"
    ) && !rules::pull_request(record)
}
pub(crate) fn shell(
    record: &Value,
    state: &Entity<AppState>,
    draft: bool,
    cx: &mut App,
) -> (gpui::Stateful<gpui::Div>, Ink) {
    let kind = field(record, "kind");
    let mut label = rules::label(record);
    let mut icon = match kind {
        "thread" => Icon::MessagesSquare,
        "environment" => Icon::Server,
        "terminal" => Icon::Terminal,
        "review-comment" => Icon::MessageCircle,
        "element" | "preview-annotation" => Icon::MousePointerClick,
        "mention" => Icon::File,
        "skill" => Icon::Sparkles,
        "image" => Icon::Image,
        "file" => Icon::File,
        _ => Icon::CircleDashed,
    };
    let mut tooltip = if kind == "unresolved" {
        "This context is no longer available.".into()
    } else {
        String::new()
    };
    let mut invalid = false;
    let mut thread = None;
    let mut url = None;
    let mut favicon = None;
    {
        let state = state.read(cx);
        if kind == "thread" {
            let local =
                state.server_config.config.as_ref().is_some_and(|c| {
                    c.environment.environment_id == field(record, "environmentId")
                });
            invalid = draft && !local;
            let shell = local
                .then(|| state.thread_shell(field(record, "threadId")))
                .flatten();
            if let Some(shell) = shell {
                if !shell.title.trim().is_empty() {
                    label = shell.title.trim().into();
                }
                thread = Some(field(record, "threadId").to_owned());
                favicon = state.sidebar.favicons.image(&shell.project_id);
                if state.shell.project(&shell.project_id).is_some() {
                    icon = Icon::FolderCode;
                }
            }
            tooltip=if invalid {"On another machine, so this machine's agent can't read it. Remove it or switch back to send."} else if shell.is_some() {"Open thread"} else {"Thread no longer available"}.into();
        } else if kind == "environment" {
            let environment = state
                .environments
                .environments
                .iter()
                .find(|e| e.environment_id.as_deref() == Some(field(record, "environmentId")));
            if let Some(e) = environment
                && !e.label.trim().is_empty()
            {
                label = e.label.trim().into();
            }
            tooltip = if environment.is_some() {
                "T3 Code environment"
            } else {
                "Machine not in this app"
            }
            .into();
            if let Some(config) = state
                .server_config
                .config
                .as_ref()
                .filter(|c| c.environment.environment_id == field(record, "environmentId"))
            {
                icon = match config
                    .settings
                    .environment_icon
                    .as_deref()
                    .or(config
                        .environment
                        .platform
                        .as_ref()
                        .map(|p| p.machine.as_str()))
                    .unwrap_or("server")
                {
                    "laptop" => Icon::Laptop,
                    "desktop" => Icon::Monitor,
                    "cloud" => Icon::Cloud,
                    "linux" => Icon::Linux,
                    "mac-mini" => Icon::MacMini,
                    "mac-studio" => Icon::MacStudio,
                    _ => Icon::Server,
                };
            }
        }
    }
    let accent_kind = if rules::pull_request(record) {
        icon = match rules::pr_state(record) {
            "merged" => Icon::GitMerge,
            "closed" => Icon::GitPullRequestClosed,
            "draft" => Icon::GitPullRequestDraft,
            _ => Icon::GitPullRequest,
        };
        let pr = &record["pullRequest"];
        let state = rules::pr_state(record);
        let term = if field(pr, "url").contains("gitlab") {
            "merge request"
        } else {
            "pull request"
        };
        let state_label = match state {
            "draft" => "Draft",
            "open" => "Open",
            "merged" => "Merged",
            "closed" => "Closed",
            _ => "Unknown",
        };
        let mut term = term.to_owned();
        term[..1].make_ascii_uppercase();
        tooltip = format!(
            "{term} {label} {state_label}\n{}\n{} → {}",
            field(pr, "title"),
            field(pr, "headBranch"),
            field(pr, "baseBranch")
        );
        if !field(pr, "url").is_empty() {
            url = Some(field(pr, "url").to_owned());
        }
        match state {
            "open" => "pr-open",
            "draft" => "pr-draft",
            "merged" => "pr-merged",
            "closed" => "pr-closed",
            _ => "pull-request",
        }
    } else {
        kind
    };
    let ink = if invalid {
        Ink::Invalid
    } else {
        Ink::Accent(accent(accent_kind, cx))
    };
    let clickable = thread.is_some() || url.is_some() || has_details(record);
    let mut shell = if let Some(image) = favicon {
        chip_shell_with_icon(
            field(record, "contextId"),
            img(image)
                .size(px(14.))
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            label,
            ink,
            clickable,
            cx,
        )
    } else {
        chip_shell(field(record, "contextId"), icon, label, ink, clickable, cx)
    }
    .tab_index(0);
    if !tooltip.is_empty() {
        shell = shell.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
    }
    let state = state.clone();
    shell = shell
        .when(clickable, |s| s.cursor_pointer())
        .on_click(move |_, _, cx| {
            if let Some(id) = &thread {
                state.update(cx, |s, cx| s.select_thread(id, cx));
            }
            if let Some(url) = &url {
                crate::pull_requests::open_context_link(&state, url, cx);
            }
        });
    (shell, ink)
}

pub(crate) fn copyable(
    shell: gpui::Stateful<gpui::Div>,
    record: &Value,
    state: &Entity<AppState>,
    _cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let record = record.clone();
    let state = state.clone();
    shell.on_action(move |_: &gpui_component::input::Copy, _, cx| {
        let env = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone())
            .unwrap_or_default();
        if let Some(text) = rules::reference(&record)
            && let Some(fragment) =
                rules::copy_fragment(&text, std::slice::from_ref(&record), &env, None, None)
        {
            crate::composer::clipboard::write(text, fragment, cx);
            cx.stop_propagation();
        }
    })
}

pub(crate) fn details(record: &Value, cx: &App) -> gpui::AnyElement {
    let p = palette(cx);
    let heading = match field(record, "kind") {
        "terminal" => field(record, "terminalLabel"),
        "preview-annotation" | "element" => field(record, "pageTitle"),
        _ => field(record, "filePath"),
    };
    let text = match field(record, "kind") {
        "preview-annotation" => field(record, "comment"),
        "element" => field(record, "htmlPreview"),
        _ => field(record, "text"),
    };
    v_flex()
        .id(SharedString::from(format!(
            "context-details-{}",
            field(record, "contextId")
        )))
        .occlude()
        .w(px(384.))
        .max_w_full()
        .max_h(px(360.))
        .overflow_y_scroll()
        .p(px(12.))
        .gap(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.dropdown_border))
        .bg(hsla(p.popover))
        .text_color(hsla(p.popover_foreground))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(heading.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(hsla(p.secondary_label))
                .child(if field(record, "kind") == "terminal" {
                    format!("Lines {}–{}", record["lineStart"], record["lineEnd"])
                } else {
                    field(record, "rangeLabel").to_owned()
                }),
        )
        .child(div().text_size(px(14.)).child(text.to_owned()))
        .when(!field(record, "diff").is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(11.))
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(field(record, "diff").to_owned()),
            )
        })
        .into_any_element()
}

pub(crate) fn copyable_message(
    shell: gpui::Stateful<gpui::Div>,
    id: String,
    source: std::sync::Arc<t3_core::turn_item::TurnItem>,
    state: &Entity<AppState>,
) -> gpui::Stateful<gpui::Div> {
    let state = state.clone();
    shell.on_action(move |_: &gpui_component::input::Copy, _, cx| {
        let t3_core::turn_item::TurnItemKind::UserMessage {
            context: Some(context),
            ..
        } = &source.kind
        else {
            return;
        };
        let Some(record) = context["records"]
            .as_array()
            .and_then(|all| all.iter().find(|r| field(r, "contextId") == id))
        else {
            return;
        };
        let environment = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone())
            .unwrap_or_default();
        if let Some(text) = rules::reference(record)
            && let Some(fragment) = rules::copy_fragment(
                &text,
                std::slice::from_ref(record),
                &environment,
                None,
                None,
            )
        {
            crate::composer::clipboard::write(text, fragment, cx);
            cx.stop_propagation();
        }
    })
}
