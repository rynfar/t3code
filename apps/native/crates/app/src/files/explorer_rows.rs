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
use t3_core::files::media_preview::DraggedFileMention;

use super::explorer::{CONTEXT, Explorer};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 12.;

pub(crate) fn file_icon(path: &str) -> Icon {
    use t3_core::files::{binary_container_kind, delimited_separator, image_kind, is_audio, is_font, is_markdown, is_mermaid, is_notebook, is_pdf, office_kind, syntax_language};
    match () {
        _ if image_kind(path).is_some() => Icon::Image,
        _ if is_audio(path) => Icon::Mic,
        _ if t3_core::media::is_workspace_video_preview_path(path) => Icon::Play,
        _ if is_font(path) => Icon::Type,
        _ if is_mermaid(path) => Icon::Workflow,
        _ if delimited_separator(path).is_some() => Icon::Rows3,
        _ if is_markdown(path) || is_pdf(path) || office_kind(path).is_some() => Icon::FileText,
        _ if binary_container_kind(path).is_some() => Icon::Archive,
        _ if syntax_language(path).is_some() || is_notebook(path) => Icon::FileBraces,
        _ => Icon::File,
    }
}

/// Floating pill rendered under the cursor while dragging a file tree item (`fileTreeDragMention.ts`).
struct DragMentionGhost {
    label: SharedString,
    kind: EntryKind,
}

impl Render for DragMentionGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let icon = if self.kind == EntryKind::Directory { Icon::Folder } else { file_icon(&self.label) };
        h_flex()
            .px(px(10.))
            .py(px(5.))
            .gap(px(6.))
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(p.primary).opacity(0.6))
            .bg(hsla(p.popover))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(p.popover_foreground))
            .child(icon.svg().size(px(13.)).text_color(hsla(p.primary)))
            .child(self.label.clone())
    }
}

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
                    .tooltip(crate::i18n::t(cx, if refreshing { "Refreshing…" } else { "Refresh files" }))
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
                        .tooltip(crate::i18n::t(cx, if all_open { "Collapse all folders" } else { "Expand all folders" }))
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
            (EntryKind::File, ..) => file_icon(&row.path),
        };
        let path = row.path.clone();
        let drag_path = row.path.clone();
        let drag_payload = DraggedFileMention::from_drag(&drag_path, &[])
            .unwrap_or_else(|| DraggedFileMention {
                paths: vec![drag_path.clone()],
                mention: t3_core::files::composer_file_link(&drag_path),
                label: row.label.clone(),
            });
        let kind = row.kind;
        let state = self.state.clone();
        let explorer_entity = cx.entity().downgrade();
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
            .on_drag(drag_payload, move |dragged, _, _, cx| {
                if let Some(explorer) = explorer_entity.upgrade() {
                    explorer.update(cx, |this, _| {
                        let _ = this.begin_drag_mention(&drag_path);
                    });
                }
                let label: SharedString = dragged.label.clone().into();
                cx.new(|_| DragMentionGhost { label, kind })
            })
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                let modified = event.modifiers().modified();
                if kind == EntryKind::Directory || modified {
                    window.focus(&this.focus, cx);
                }
                this.activate_row(index, !modified, cx);
            }))
            .child(
                icon.svg()
                    .flex_none()
                    .size(px(if kind == EntryKind::File { 14. } else { 12. }))
                    .text_color(hsla(p.icon_muted))
                    .when(row.loading, |svg| svg.opacity(0.6)),
            )
            .child(div().min_w_0().truncate().when(selected, |this| this.font_weight(FontWeight::MEDIUM)).child(label))
            .context_menu(move |menu, _, cx| {
                let item = |label: SharedString, run: fn(&mut crate::app_state::AppState, &str, &mut Context<crate::app_state::AppState>)| {
                    let state = state.clone();
                    let path = path.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| state.update(cx, |state, cx| run(state, &path, cx)))
                };
                let menu = if let Some(editor) = state.read(cx).preferred_editor_label() {
                    menu.item(item(crate::i18n::t_fmt(cx, "Open in {editor}", &[("editor", editor.into())]).into(), |state, path, cx| {
                        state.open_file_in_preferred_editor(path, None, cx);
                    }))
                } else {
                    menu
                };
                menu.item(item(crate::i18n::t(cx, "Copy path").into(), |state, path, cx| state.copy_file_path(path, cx)))
                    .item(item(crate::i18n::t(cx, "Copy mention").into(), |state, path, cx| state.copy_file_mention(path, cx)))
                    .item(item(crate::i18n::t(cx, "Add to chat").into(), |state, path, cx| state.add_file_to_chat(path, cx)))
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
        if !cx.has_active_drag() && self.drag_mention.is_drag_in_progress() {
            let _ = self.drag_mention.end_drag();
        }
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
            .id("file-explorer-root")
            .key_context(CONTEXT)
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .on_action(cx.listener(Self::on_focus_search))
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
                    .child(crate::i18n::t_fmt(cx, "{error} Click to retry.", &[("error", error.into())]))
            }))
            .when(searching && self.search.truncated && !self.search.pending, |this| {
                this.child(self.render_status(crate::i18n::t(cx, "More matches available. Refine your search.").to_owned(), false, cx))
            })
            .when(loading && count == 0, |this| this.child(self.render_status(crate::i18n::t(cx, "Loading files…").to_owned(), false, cx)))
            .when(no_matches, |this| this.child(self.render_status(crate::i18n::t(cx, "No matching files.").to_owned(), true, cx)))
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
            .when(cx.has_active_drag() && self.drag_mention.is_drag_in_progress(), |this| {
                let border = hsla(p.primary).opacity(0.6);
                let hover_bg = hsla(p.primary).opacity(0.12);
                this.child(
                    h_flex()
                        .id("file-tree-drop-mention")
                        .flex_none()
                        .h(px(34.))
                        .mx(px(8.))
                        .mb(px(8.))
                        .px(px(10.))
                        .gap(px(6.))
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(border)
                        .bg(hsla(p.secondary))
                        .text_xs()
                        .text_color(hsla(p.foreground))
                        .drag_over::<DraggedFileMention>(move |style, _, _, _| {
                            style.bg(hover_bg).border_color(border)
                        })
                        .on_drop(cx.listener(|this, dragged: &DraggedFileMention, _, cx| {
                            this.drop_mention_to_chat(dragged, cx);
                        }))
                        .child(Icon::MessageSquarePlus.svg().size(px(13.)).text_color(hsla(p.primary)))
                        .child("Drop here to mention in chat"),
                )
            })
    }
}
