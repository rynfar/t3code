//! The changed-files tree beside the code view (`components/diffs/DiffFileTree.tsx` in Pierre's
//! compact density): selecting a file reveals it in the list. Rows are a `uniform_list`.

use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, relative, uniform_list};
use gpui_component::{h_flex, v_flex};
use t3_core::diff::{ChangeKind, TreeRowKind};

use super::DiffPanel;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 24.;

pub fn render(panel: &mut DiffPanel, cx: &mut Context<DiffPanel>) -> impl IntoElement {
    let p = palette(cx);
    let count = panel.view.as_ref().map_or(0, |view| view.parsed.files().len());
    let rows = panel.view.as_ref().map_or(0, |view| view.tree.len());
    v_flex()
        .flex_none()
        .w(relative(0.4))
        .min_w(px(160.))
        .max_w(px(360.))
        .h_full()
        .border_l_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.background))
        .child(
            h_flex()
                .flex_none()
                .h(px(28.))
                .px(px(8.))
                .gap(px(4.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(div().px(px(4.)).font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Files"))
                .child(div().ml_auto().child(count.to_string())),
        )
        .child(
            uniform_list("diff-file-tree", rows, cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range.map(|ix| this.render_tree_row(ix, cx)).collect::<Vec<_>>()
            }))
            .flex_1()
            .track_scroll(&panel.tree_scroll),
        )
}

impl DiffPanel {
    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.view.as_ref().and_then(|view| view.tree.get(ix)) else { return div().into_any_element() };
        let p = palette(cx);
        let base = h_flex()
            .id(("diff-tree-row", ix))
            .h(px(ROW_HEIGHT))
            .mx(px(4.))
            .pl(px(8. + 12. * row.depth as f32))
            .pr(px(8.))
            .gap(px(4.))
            .rounded(px(4.))
            .text_xs()
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.foreground).opacity(0.07)));
        let label: SharedString = row.label.clone().into();
        match &row.kind {
            TreeRowKind::Directory { path, expanded } => {
                let path = path.clone();
                base.text_color(hsla(p.muted_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_directory(&path, cx)))
                    .child(
                        (if *expanded { Icon::ChevronDown } else { Icon::ChevronRight })
                            .svg()
                            .size(px(12.))
                            .text_color(hsla(p.icon_muted)),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .into_any_element()
            }
            TreeRowKind::File { file, change } => {
                let file = *file;
                let selected = self
                    .view
                    .as_ref()
                    .and_then(|view| view.parsed.files().get(file))
                    .is_some_and(|diff| self.selected_file.as_deref() == Some(diff.path()));
                let (letter, color) = match change {
                    ChangeKind::New => ("A", p.diff_addition_foreground),
                    ChangeKind::Deleted => ("D", p.diff_deletion_foreground),
                    ChangeKind::RenamePure | ChangeKind::RenameChanged => ("R", p.diff_modified_icon),
                    ChangeKind::Modified => ("M", p.diff_modified_icon),
                };
                base.pl(px(8. + 12. * row.depth as f32 + 16.))
                    .text_color(hsla(p.foreground).opacity(0.85))
                    .when(selected, |this| this.bg(hsla(p.foreground).opacity(0.12)))
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_file(file, cx)))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().text_size(px(11.)).text_color(hsla(color)).child(letter))
                    .into_any_element()
            }
        }
    }
}
