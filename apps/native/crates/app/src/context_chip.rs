//! Shared context presentation from `ThreadContextChip.tsx`, `EnvironmentContextChip.tsx`,
//! `contextChipParts.tsx` and `chat/UserMessageContextChips.tsx`.
use crate::{
    app_state::AppState,
    composer::context::{Ink, chip_shell, chip_shell_with_icon},
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{
    App, Entity, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, tooltip::Tooltip, v_flex};
use serde_json::Value;
use t3_core::composer::records::{self as rules, field};

pub(crate) fn environment_machine_icon(state: &AppState, environment_id: &str) -> Icon {
    let saved = state.environments.environments.iter().find(|saved| {
        saved.id == environment_id || saved.environment_id.as_deref() == Some(environment_id)
    });
    let config = state.fleet.environments.iter().find(|entry| {
        let owner = &entry.summary.owner;
        owner.saved_id == environment_id
            || owner.canonical_id.as_deref() == Some(environment_id)
            || saved.is_some_and(|saved| owner.saved_id == saved.id)
    }).and_then(|entry| entry.config.config.as_ref());
    config.or_else(|| state.server_config.config.as_ref().filter(|config| {
        config.environment.environment_id == environment_id
            || state.environments.active.as_deref() == Some(environment_id)
            || saved.is_some_and(|saved| state.environments.active.as_deref() == Some(&saved.id))
    })).map_or(Icon::Server, Icon::machine)
}

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
    let kind = field(record, "kind");
    (matches!(kind, "terminal" | "element" | "preview-annotation" | "review-comment")
        && !rules::pull_request(record))
        || (kind == "skill"
            && (!field(record, "description").is_empty() || !field(record, "path").is_empty()))
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
    let mention_path = (kind == "mention")
        .then(|| field(record, "path").trim().to_owned())
        .filter(|path| !path.is_empty());
    let mut tooltip = if kind == "unresolved" {
        "This context is no longer available.".into()
    } else if let Some(path) = &mention_path {
        format!("{path}\nClick to copy path")
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
                favicon = state.shell.project(&shell.project_id).map(|_| crate::project_icon::ProjectVisual::for_project(state, &shell.project_id));
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
                .find(|e| e.id == field(record, "environmentId") || e.environment_id.as_deref() == Some(field(record, "environmentId")));
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
            icon = environment_machine_icon(state, field(record, "environmentId"));
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
    let clickable = thread.is_some() || url.is_some() || mention_path.is_some() || has_details(record);
    let mut shell = if let Some(visual) = favicon {
        chip_shell_with_icon(
            field(record, "contextId"),
            visual.render(crate::project_icon::size(14., cx), crate::theme::hsla(crate::theme::palette(cx).icon_muted), cx),
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
            if let Some(path) = &mention_path {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                state.update(cx, |s, cx| {
                    s.show_toast(
                        crate::toast::Toast::new(crate::toast::ToastKind::Success, "Copied file path")
                            .description(path.clone()),
                        cx,
                    );
                });
            }
        });
    (shell, ink)
}

pub(crate) fn attachment_shell(record: &Value, attachment: &Value, state: &Entity<AppState>, cx: &mut App) -> gpui::Stateful<gpui::Div> {
    let kind = field(record, "kind");
    let is_video = kind == "file" && t3_core::media::is_video_attachment(attachment);
    let downloadable = attachment.get("downloadable").and_then(Value::as_bool) != Some(false);
    let disabled = kind == "file" && !downloadable && !is_video;
    let display_name = if kind == "file" {
        t3_core::timeline::user_message_file_label(attachment)
    } else {
        field(record, "name").to_owned()
    };
    let mut record_for_shell = record.clone();
    if kind == "file" && !display_name.is_empty() {
        record_for_shell["label"] = serde_json::json!(display_name);
    }
    let (mut shell, _) = shell(&record_for_shell, state, false, cx);
    if is_video {
        shell = shell.text_color(hsla(accent("video", cx)));
    }
    if let Some(size) = t3_core::media::attachment_size_label(record, attachment) {
        let tooltip = format!("{display_name}\n{size}");
        shell = shell.child(div().flex_shrink_0().text_size(crate::theme::rem(crate::theme::metrics::TEXT_3XS)).child(size))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
    }
    let record_attachment_id = field(record, "attachmentId");
    let attachment_id = if record_attachment_id.is_empty() {
        field(attachment, "id").to_owned()
    } else {
        record_attachment_id.to_owned()
    };
    if disabled {
        shell = shell.opacity(0.6);
    } else if !attachment_id.is_empty() {
        let is_image = kind == "image";
        let name = display_name;
        shell = shell.cursor_pointer().on_click(move |_, window, cx| {
            let resource = t3_core::media::AssetResource::Attachment { attachment_id: attachment_id.clone() };
            if (is_image || is_video)
                && let Some(viewer) = cx
                    .try_global::<crate::media::Media>()
                    .and_then(|media| media.scope.as_ref().and_then(|scope| scope.viewer.upgrade()))
            {
                let scope = cx.try_global::<crate::media::Media>().and_then(|media| media.scope.clone());
                let source = t3_core::media::MediaSource::Asset(resource);
                let item = if is_video {
                    crate::media::ViewerItem::video(source, name.clone())
                } else {
                    crate::media::ViewerItem::image(source, name.clone())
                };
                viewer.update(cx, |viewer, cx| viewer.open_with_scope(vec![item], 0, scope, window, cx));
                return;
            }
            crate::media::open_asset(&resource, cx);
        });
    }
    shell
}

pub(crate) fn copyable_unresolved(shell: gpui::Stateful<gpui::Div>, record: &Value) -> gpui::Stateful<gpui::Div> {
    let text=rules::reference(record).unwrap_or_default();
    shell.on_action(move|_: &gpui_component::input::Copy,_,cx| {crate::composer::clipboard::write(text.clone(),String::new(),cx);cx.stop_propagation();})
}

pub(crate) fn render_diff_lines(diff: &str, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let mono = cx.theme().mono_font_family.clone();
    let lines: Vec<_> = diff
        .lines()
        .map(|line| {
            let (fg, bg) = if line.starts_with('+') {
                (hsla(p.diff_addition_foreground), hsla(p.diff_addition_foreground).opacity(0.15))
            } else if line.starts_with('-') {
                (hsla(p.diff_deletion_foreground), hsla(p.diff_deletion_foreground).opacity(0.15))
            } else {
                (hsla(p.muted_foreground), gpui::transparent_black())
            };
            div()
                .px(px(4.))
                .bg(bg)
                .text_color(fg)
                .font_family(mono.clone())
                .text_size(px(11.))
                .child(line.to_owned())
        })
        .collect();
    v_flex()
        .p(px(6.))
        .rounded(px(6.))
        .bg(hsla(p.secondary).opacity(0.45))
        .children(lines)
}

fn page_url_field(record: &Value) -> &str {
    let page_url = field(record, "pageUrl");
    if !page_url.is_empty() { page_url } else { field(record, "url") }
}

fn source_label(record: &Value) -> Option<String> {
    let source = record.get("source")?.as_object()?;
    let file_name = source.get("fileName").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    match source.get("lineNumber").and_then(Value::as_u64) {
        Some(line) => Some(format!("{file_name}:{line}")),
        None => Some(file_name.to_owned()),
    }
}

fn terminal_line_caption(record: &Value) -> String {
    let start = &record["lineStart"];
    let end = &record["lineEnd"];
    if start == end {
        format!("Line {start}")
    } else {
        format!("Lines {start}–{end}")
    }
}

pub(crate) fn details(record: &Value, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    let kind = field(record, "kind");
    let page_url = page_url_field(record);
    let heading = match kind {
        "terminal" => field(record, "terminalLabel").to_owned(),
        "preview-annotation" => {
            let title = field(record, "pageTitle").trim();
            if !title.is_empty() { title.to_owned() } else if !page_url.is_empty() { page_url.to_owned() } else { "Preview annotation".to_owned() }
        }
        "element" => {
            let title = field(record, "pageTitle").trim();
            if !title.is_empty() { title.to_owned() } else if !page_url.is_empty() { page_url.to_owned() } else { "Selected element".to_owned() }
        }
        "skill" => {
            let label = field(record, "label");
            if !label.is_empty() { label.to_owned() } else { field(record, "name").to_owned() }
        }
        _ => field(record, "filePath").to_owned(),
    };
    let subtitle = match kind {
        "terminal" => terminal_line_caption(record),
        "review-comment" => {
            let section = field(record, "sectionTitle");
            let range = field(record, "rangeLabel");
            if !section.is_empty() && !range.is_empty() {
                format!("{section} · {range}")
            } else if !section.is_empty() {
                section.to_owned()
            } else {
                range.to_owned()
            }
        }
        "preview-annotation" | "element" => {
            let selector = field(record, "selector");
            if !selector.is_empty() && !page_url.is_empty() {
                format!("{selector} · {page_url}")
            } else if !selector.is_empty() {
                selector.to_owned()
            } else {
                page_url.to_owned()
            }
        }
        "skill" => field(record, "path").to_owned(),
        _ => field(record, "rangeLabel").to_owned(),
    };
    let text = match kind {
        "preview-annotation" => field(record, "comment"),
        "element" => "",
        "skill" => field(record, "description"),
        _ => field(record, "text"),
    };
    let html_preview = if kind == "element" { field(record, "htmlPreview") } else { "" };
    let styles = if kind == "element" { field(record, "styles").trim() } else { "" };
    let target_summary = if kind == "preview-annotation" { field(record, "targetSummary") } else { "" };
    let style_change_count = if kind == "preview-annotation" {
        record.get("styleChanges").and_then(Value::as_array).map_or(0, Vec::len)
    } else {
        0
    };
    let annotation_elements: Vec<&Value> = if kind == "preview-annotation" {
        record.get("elements").and_then(Value::as_array).map(|arr| arr.iter().collect()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let element_source = (kind == "element").then(|| source_label(record)).flatten();
    let diff = field(record, "diff");
    let has_screenshot = kind == "preview-annotation"
        && (!field(record, "attachmentId").is_empty()
            || !field(&record["attachment"], "id").is_empty()
            || !field(record, "screenshotContextId").is_empty());
    let mono = cx.theme().mono_font_family.clone();
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
        .shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark()))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(heading),
        )
        .when(!subtitle.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(11.))
                    .text_color(hsla(p.secondary_label))
                    .child(subtitle),
            )
        })
        .when_some(element_source, |s, src| {
            s.child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(src))
        })
        .when(has_screenshot, |s| {
            s.child(
                div()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(hsla(p.secondary))
                    .text_size(px(11.))
                    .text_color(hsla(p.muted_foreground))
                    .child("Screenshot attached"),
            )
        })
        .when(!text.is_empty(), |s| s.child(div().text_size(px(13.)).child(text.to_owned())))
        .when(!target_summary.is_empty() || style_change_count > 0, |s| {
            s.child(
                h_flex()
                    .gap(px(8.))
                    .text_size(px(11.))
                    .text_color(hsla(p.secondary_label))
                    .when(!target_summary.is_empty(), |row| row.child(div().min_w_0().truncate().child(target_summary.to_owned())))
                    .when(style_change_count > 0, |row| row.child(format!("{style_change_count} style changes"))),
            )
        })
        .when(!annotation_elements.is_empty(), |s| {
            let mono = mono.clone();
            s.child(
                v_flex()
                    .gap(px(6.))
                    .pt(px(6.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .children(annotation_elements.into_iter().map(move |el| {
                        let selector = field(el, "selector");
                        let tag = field(el, "tagName");
                        let name = if !selector.is_empty() {
                            selector.to_owned()
                        } else if !tag.is_empty() {
                            format!("<{tag}>")
                        } else {
                            "element".to_owned()
                        };
                        let el_source = source_label(el);
                        let el_html = field(el, "htmlPreview").trim().to_owned();
                        v_flex()
                            .gap(px(4.))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap(px(8.))
                                    .text_size(px(11.))
                                    .child(div().min_w_0().truncate().font_family(mono.clone()).child(name))
                                    .when_some(el_source, |row, src| {
                                        row.child(div().ml_auto().flex_shrink_0().text_color(hsla(p.secondary_label)).child(src))
                                    }),
                            )
                            .when(!el_html.is_empty(), |box_div| {
                                box_div.child(
                                    div()
                                        .p(px(6.))
                                        .rounded(px(4.))
                                        .bg(hsla(p.secondary).opacity(0.5))
                                        .font_family(mono.clone())
                                        .text_size(px(10.))
                                        .child(el_html),
                                )
                            })
                    })),
            )
        })
        .when(!html_preview.is_empty(), |s| {
            s.child(
                div()
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(hsla(p.secondary).opacity(0.6))
                    .font_family(mono.clone())
                    .text_size(px(11.))
                    .child(html_preview.to_owned()),
            )
        })
        .when(!styles.is_empty(), |s| {
            s.child(
                div()
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(hsla(p.secondary).opacity(0.45))
                    .font_family(mono.clone())
                    .text_size(px(11.))
                    .child(styles.to_owned()),
            )
        })
        .when(!diff.is_empty(), |s| s.child(render_diff_lines(diff, cx)))
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
        let Some(all)=context["records"].as_array() else {return};
        let environment = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone())
            .unwrap_or_default();
        if let Some((text,fragment))=crate::composer::imports::copy_chip(&id,all,&environment) {
            crate::composer::clipboard::write(text,fragment,cx);cx.stop_propagation();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn has_details_and_skill_element_preview_annotation_fields() {
        assert!(has_details(&json!({"kind": "terminal"})));
        assert!(has_details(&json!({"kind": "preview-annotation"})));
        assert!(has_details(&json!({"kind": "element"})));
        assert!(has_details(&json!({"kind": "review-comment"})));
        assert!(!has_details(&json!({"kind": "review-comment", "pullRequest": {"number": 12}})));
        assert!(has_details(&json!({"kind": "skill", "name": "deploy", "description": "Ship build"})));
        assert!(!has_details(&json!({"kind": "skill", "name": "empty"})));

        assert_eq!(
            page_url_field(&json!({"pageUrl": "https://localhost:3000/home"})),
            "https://localhost:3000/home"
        );
        assert_eq!(
            source_label(&json!({"source": {"fileName": "App.tsx", "lineNumber": 42}})).as_deref(),
            Some("App.tsx:42")
        );
        assert_eq!(
            terminal_line_caption(&json!({"lineStart": 7, "lineEnd": 7})),
            "Line 7"
        );
        assert_eq!(
            terminal_line_caption(&json!({"lineStart": 7, "lineEnd": 10})),
            "Lines 7–10"
        );
    }
}
