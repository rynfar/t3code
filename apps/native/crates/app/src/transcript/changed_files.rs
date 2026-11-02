//! The changed-files card under a turn's last assistant message (`ChangedFilesCard` in
//! `ChangedFilesTree.tsx`, design system §7.8): the count and line stats, Expand/Collapse all
//! folders, Open diff, then the tree. A file opens the diff panel on that turn and file.

use gpui::{AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::diff::TurnDiffSummary;
use t3_core::timeline::{DiffStat, TreeNode, changed_file_rows, changed_file_tree, changed_files_label, has_directories, hidden_files_label};

use super::RowEnv;
use super::parts::{focusable, ghost_icon_button, ghost_text_button};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;

/// `DiffStatLabel`: `+N` in the addition color, `-N` in the deletion color, mono. `aligned` puts
/// them in two 4ch columns.
fn stat_label(stat: DiffStat, aligned: bool, size: f32, cx: &App) -> AnyElement {
    let p = palette(cx);
    let mono = cx.theme().mono_font_family.clone();
    let cell = |text: String, color: u32| {
        let cell = div().text_color(hsla(color)).child(text);
        if aligned { cell.w(px(size * 2.4)).flex().justify_end() } else { cell }
    };
    h_flex()
        .flex_none()
        .gap(px(if aligned { 8. } else { 4. }))
        .font_family(mono)
        .text_size(px(size))
        .child(cell(format!("+{}", t3_core::diff::compact_count(stat.additions)), p.diff_addition_foreground))
        .child(cell(format!("-{}", t3_core::diff::compact_count(stat.deletions)), p.diff_deletion_foreground))
        .into_any_element()
}

pub fn changed_files_card(message_id: &str, summary: &TurnDiffSummary, env: &RowEnv, cx: &mut Context<ThreadView>) -> Option<AnyElement> {
    if summary.files.is_empty() {
        return None;
    }
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let mono = cx.theme().mono_font_family.clone();
    let run_id = summary.run_id.clone();
    let totals = summary.totals();
    let tree = changed_file_tree(&summary.files);
    let directories = has_directories(&tree);
    let all_expanded = env.ui.changed_files_open.contains(&run_id);
    let overrides = env.ui.changed_file_dirs.get(&run_id).cloned().unwrap_or_default();
    let first_path = summary.files.first().map(|file| file.path.clone());
    let fill = if dark { hsla(p.input).opacity(0.2) } else { hsla(p.secondary) };

    let open_diff = {
        let (run_id, path) = (run_id.clone(), first_path.clone());
        move |view: &mut ThreadView, cx: &mut Context<ThreadView>| view.open_turn_diff(&run_id, path.as_deref(), cx)
    };
    let mut header_actions: Vec<AnyElement> = Vec::new();
    if directories {
        let (run_id, message_id) = (run_id.clone(), message_id.to_owned());
        header_actions.push(ghost_icon_button(
            SharedString::from(format!("changed-files-all:{run_id}")),
            if all_expanded { Icon::ChevronsDownUp } else { Icon::ChevronsUpDown },
            cx.listener(move |view, _, _, cx| view.toggle_changed_files_all(&run_id, &message_id, cx)),
            cx,
        ));
    }
    let open = open_diff.clone();
    header_actions.push(ghost_text_button(
        SharedString::from(format!("changed-files-open:{run_id}")),
        "Open diff",
        Some(Icon::FileDiff),
        cx.listener(move |view, _, _, cx| open(view, cx)),
        cx,
    ));
    let header = h_flex()
        .justify_between()
        .gap(px(8.))
        .px(px(12.))
        .py(px(8.))
        .child(
            h_flex()
                .min_w_0()
                .flex_wrap()
                .gap(px(12.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(changed_files_label(totals.count))
                .when(totals.additions > 0 || totals.deletions > 0, |this| this.child(stat_label(DiffStat::from(totals), false, 12., cx))),
        )
        .child(h_flex().flex_none().gap(px(4.)).children(header_actions));

    let rows = changed_file_rows(&tree, &overrides, all_expanded);
    let row_base = |id: SharedString, depth: usize| {
        h_flex().id(id).h(px(28.)).w_full().gap(px(8.)).pl(px(8. + 14. * depth as f32)).pr(px(8.)).rounded(px(6.)).cursor_pointer().hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
    };
    let items: Vec<AnyElement> = rows
        .iter()
        .map(|row| match row.node {
            TreeNode::Directory { name, path, stat, .. } => {
                let (run_id, message_id, path) = (run_id.clone(), message_id.to_owned(), path.clone());
                let element = row_base(SharedString::from(format!("changed-dir:{run_id}:{path}")), row.depth)
                    .child((if row.expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(14.), hsla(p.muted_foreground).opacity(0.7)))
                    .child(Icon::Folder.render(px(14.), hsla(p.muted_foreground).opacity(0.75)))
                    .child(div().min_w_0().truncate().font_family(mono.clone()).text_size(px(11.)).text_color(hsla(p.muted_foreground).opacity(0.9)).child(name.clone()))
                    .when(!stat.is_zero(), |this| this.child(div().ml_auto().child(stat_label(*stat, true, 10., cx))))
                    .on_click(cx.listener(move |view, _, _, cx| view.toggle_changed_dir(&run_id, &path, all_expanded, &message_id, cx)));
                focusable(element, cx).into_any_element()
            }
            TreeNode::File { name, path, stat } => {
                let (run_id, file) = (run_id.clone(), path.clone());
                let element = row_base(SharedString::from(format!("changed-file:{run_id}:{path}")), row.depth)
                    .when(directories || row.depth > 0, |this| this.child(div().flex_none().size(px(14.))))
                    .child(div().min_w_0().truncate().font_family(mono.clone()).text_xs().text_color(hsla(p.foreground).opacity(0.85)).child(name.clone()))
                    .child(div().ml_auto().child(stat_label(*stat, true, 10., cx)))
                    .on_click(cx.listener(move |view, _, _, cx| view.open_turn_diff(&run_id, Some(&file), cx)));
                focusable(element, cx).into_any_element()
            }
        })
        .collect();
    let hidden = totals.count.saturating_sub(summary.files.len() as u64);
    Some(
        v_flex()
            .mt(px(16.))
            .rounded(px(8.))
            .bg(fill)
            .child(header)
            .child(v_flex().p(px(8.)).children(items))
            .when(hidden > 0, |this| {
                this.child(div().px(px(12.)).pb(px(8.)).child(ghost_text_button(
                    SharedString::from(format!("changed-files-more:{run_id}")),
                    hidden_files_label(hidden),
                    None,
                    cx.listener(move |view, _, _, cx| open_diff(view, cx)),
                    cx,
                )))
            })
            .into_any_element(),
    )
}
