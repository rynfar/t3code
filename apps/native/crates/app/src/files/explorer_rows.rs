//! The explorer's look: the subheader (refresh, "Search files", expand/collapse all) and the
//! states under it from `FileBrowserPanel.tsx`, then Pierre's compact rows as
//! `pierre-tree-theme.ts` styles them: 24px, 12px text, hover at 7% and selection at 12% of the
//! text color, radius 6. The row menu offers the mention actions.

use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::files::EntryKind;

use super::explorer::{CONTEXT, Explorer};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 12.;

impl Explorer {
    fn render_subheader(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let refreshing = self.is_refreshing();
        let all_open = self.tree.expanding_all() || self.tree.all_expanded();
        h_flex()
            .flex_none()
            .h(px(36.))
            .mb(px(4.))
            .px(px(8.))
            .gap(px(4.))
            .child(
                Button::new("files-refresh")
                    .variant(Variant::Ghost)
                    .size(Size::IconXs)
                    .icon(Icon::RefreshCw)
                    .tooltip(if refreshing { "Refreshing…" } else { "Refresh files" })
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                div()
                    .id("files-search")
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .text_sm()
                    .child(Input::new(&self.search_input).appearance(false).bordered(false).small()),
            )
            .when(self.tree.has_directories(), |this| {
                this.child(
                    Button::new("files-toggle-all")
                        .variant(Variant::Ghost)
                        .size(Size::IconXs)
                        .icon(if all_open { Icon::ChevronsDownUp } else { Icon::ChevronsUpDown })
                        .tooltip(if all_open { "Collapse all folders" } else { "Expand all folders" })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_all(cx))),
                )
            })
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index) else { return div().into_any_element() };
        let p = palette(cx);
        let selected = self.selected.as_deref() == Some(row.path.as_str());
        let cursor = self.cursor.as_deref() == Some(row.path.as_str()) && self.tree_focused;
        let label: SharedString = row.label.clone().into();
        let text = if row.ignored { hsla(p.muted_foreground).opacity(0.7) } else { hsla(p.foreground) };
        let icon = match (row.kind, row.expanded, row.loading) {
            (EntryKind::Directory, _, true) => Icon::LoaderCircle,
            (EntryKind::Directory, true, false) => Icon::ChevronDown,
            (EntryKind::Directory, false, false) => Icon::ChevronRight,
            (EntryKind::File, ..) => Icon::File,
        };
        let path = row.path.clone();
        let kind = row.kind;
        let state = self.state.clone();
        h_flex()
            .id(ElementId::from(("file-tree-row", index)))
            .h(px(ROW_HEIGHT))
            .mx(px(4.))
            .pl(px(8. + INDENT * row.depth as f32))
            .pr(px(8.))
            .gap(px(6.))
            .rounded(px(6.))
            .text_xs()
            .text_color(text)
            .cursor_pointer()
            .when(selected, |this| this.bg(hsla(p.foreground).opacity(0.12)))
            .when(!selected && cursor, |this| this.bg(hsla(p.foreground).opacity(0.07)))
            .when(!selected, |this| this.hover(|style| style.bg(hsla(p.foreground).opacity(0.07))))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.activate_row(index, cx);
            }))
            .child(
                icon.svg()
                    .flex_none()
                    .size(px(if kind == EntryKind::File { 14. } else { 12. }))
                    .text_color(hsla(p.icon_muted))
                    .when(row.loading, |svg| svg.opacity(0.6)),
            )
            .child(div().min_w_0().truncate().when(selected, |this| this.font_weight(FontWeight::MEDIUM)).child(label))
            .context_menu(move |menu, _, _| {
                let item = |label: &'static str, run: fn(&mut crate::app_state::AppState, &str, &mut Context<crate::app_state::AppState>)| {
                    let state = state.clone();
                    let path = path.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| state.update(cx, |state, cx| run(state, &path, cx)))
                };
                menu.item(item("Copy mention", |state, path, cx| state.copy_file_mention(path, cx)))
                    .item(item("Add to chat", |state, path, cx| state.add_file_to_chat(path, cx)))
            })
            .into_any_element()
    }

    fn render_status(&self, text: String, padded: bool, cx: &Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .flex_none()
            .px(px(12.))
            .py(px(if padded { 16. } else { 4. }))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(text)
    }
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.pending_reset_input) {
            self.search_input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.sync_rows();
        self.tree_focused = self.focus.is_focused(window);
        let p = palette(cx);
        let searching = !self.search.query.trim().is_empty();
        let error = self.tree.error().map(str::to_owned).or_else(|| self.search.error.clone());
        let loading = !self.tree.ready() || self.search.pending;
        let no_matches = searching && self.rows.is_empty() && !loading && error.is_none();
        let count = self.rows.len();
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(self.render_subheader(cx))
            .children(error.map(|error| {
                div()
                    .id("files-error")
                    .flex_none()
                    .p(px(16.))
                    .text_xs()
                    .line_height(px(19.5))
                    .text_color(hsla(p.destructive))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                    .child(format!("{error} Click to retry."))
            }))
            .when(searching && self.search.truncated && !self.search.pending, |this| {
                this.child(self.render_status("More matches available. Refine your search.".to_owned(), false, cx))
            })
            .when(loading && count == 0, |this| this.child(self.render_status("Loading files…".to_owned(), false, cx)))
            .when(no_matches, |this| this.child(self.render_status("No matching files.".to_owned(), true, cx)))
            .when(!no_matches, |this| {
                this.child(
                    div()
                        .id("file-tree")
                        .key_context(CONTEXT)
                        .track_focus(&self.focus)
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .on_action(cx.listener(Self::select_previous))
                        .on_action(cx.listener(Self::select_next))
                        .on_action(cx.listener(Self::expand_selected))
                        .on_action(cx.listener(Self::collapse_selected))
                        .on_action(cx.listener(Self::open_selected))
                        .child(
                            uniform_list("file-tree-rows", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()
                            }))
                            .size_full()
                            .pb(px(8.))
                            .track_scroll(&self.scroll),
                        )
                        .vertical_scrollbar(&self.scroll),
                )
            })
    }
}
