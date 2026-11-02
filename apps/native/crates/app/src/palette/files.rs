//! Go-to-file mode (`apps/web/src/components/files/ProjectFilePicker.tsx`): files of the active
//! project from `projects.searchEntries`, server order kept, matched letters bolded.

use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled, StyledText, Task, UniformListScrollHandle, div, prelude::*, px, uniform_list};
use t3_core::project_search::{EntriesResult, FilePickerMatch, SEARCH_DEBOUNCE_MS, SEARCH_ENTRIES, entries_request, file_picker_matches};

use super::CommandPalette;
use crate::app_state::AppState;
use crate::project_files::ProjectTarget;
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 32.;

#[derive(Default)]
pub(super) struct FileSearch {
    pub target: Option<ProjectTarget>,
    pub matches: Vec<FilePickerMatch>,
    matched_query: String,
    pending: bool,
    error: Option<String>,
    pub highlighted: usize,
    pub scroll: UniformListScrollHandle,
    request: Option<Task<()>>,
}

/// Byte ranges for char indices, merged into runs.
pub(super) fn char_ranges(text: &str, indices: &[usize]) -> Vec<std::ops::Range<usize>> {
    let offsets: Vec<(usize, usize)> = text.char_indices().map(|(start, c)| (start, start + c.len_utf8())).collect();
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for index in indices {
        let Some(&(start, end)) = offsets.get(*index) else { continue };
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    ranges
}

impl FileSearch {
    pub fn start(&mut self, target: Option<ProjectTarget>, state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        *self = FileSearch { target, ..Default::default() };
        self.search(String::new(), state, cx);
    }

    pub fn search(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        let Some(target) = self.target.clone() else { return };
        self.pending = true;
        let request = state.read(cx).rpc(SEARCH_ENTRIES, entries_request(&target.cwd, &query));
        self.request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
            let result = request.await;
            let _ = this.update(cx, |palette, cx| {
                let files = &mut palette.files;
                files.pending = false;
                match result.map(serde_json::from_value::<EntriesResult>) {
                    Ok(Ok(result)) => {
                        files.matches = file_picker_matches(&result.entries, &query);
                        files.matched_query = query;
                        files.error = None;
                    }
                    Ok(Err(error)) => files.error = Some(error.to_string()),
                    Err(error) => files.error = Some(error.to_string()),
                }
                files.highlighted = 0;
                cx.notify();
            });
        }));
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.matches.len() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
            self.scroll.scroll_to_item(self.highlighted, gpui::ScrollStrategy::Top);
        }
    }

    pub fn highlighted_path(&self) -> Option<String> {
        self.matches.get(self.highlighted).map(|found| found.path.clone())
    }

    fn empty_message(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        let searching = !self.matched_query.trim().is_empty();
        match (self.pending, searching) {
            (true, true) => "Searching workspace files…",
            (true, false) => "Indexing workspace files…",
            (false, true) => "No matching files.",
            (false, false) => "No files found.",
        }
        .to_owned()
    }

    pub fn render(&self, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let Some(target) = &self.target else {
            return div().py(px(40.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("Open a project to search its files.").into_any_element();
        };
        if self.matches.is_empty() {
            return div().py(px(40.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child(self.empty_message()).into_any_element();
        }
        let count = self.matches.len();
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .p(px(8.))
            .child(div().px(px(8.)).pt(px(4.)).pb(px(4.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(target.project_name.clone()))
            .child(
                uniform_list("palette-files", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.files.render_row(index, cx)).collect::<Vec<_>>()))
                    .track_scroll(&self.scroll)
                    .h(px((count as f32 * ROW_HEIGHT).min(480.))),
            )
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let Some(found) = self.matches.get(index) else { return div().into_any_element() };
        let active = !self.matched_query.trim().is_empty();
        let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(hsla(p.foreground)), ..Default::default() };
        let styled = |text: &str, indices: &[usize]| {
            let ranges = if active { char_ranges(text, indices) } else { Vec::new() };
            StyledText::new(SharedString::from(text.to_owned())).with_highlights(ranges.into_iter().map(move |range| (range, bold)))
        };
        let highlighted = index == self.highlighted;
        div()
            .id(("palette-file", index))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)))
            .child(div().flex_none().text_sm().text_color(hsla(if active { p.muted_foreground } else { p.foreground })).child(styled(&found.name, &found.name_match_indices)))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(styled(&found.path, &found.path_match_indices)))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.files.highlighted != index {
                    this.files.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.files.highlighted = index;
                this.open_file(event.modifiers().platform, window, cx);
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn char_ranges_merge_adjacent_indices_by_byte() {
        assert_eq!(super::char_ranges("app.rs", &[0, 1, 4]), [0..2, 4..5]);
        assert_eq!(super::char_ranges("é/ab", &[0, 2]), [0..2, 3..4]);
    }
}
