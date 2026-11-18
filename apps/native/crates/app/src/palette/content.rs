//! Project content search mode (`apps/web/src/components/search/ProjectContentSearchDialog.tsx`):
//! `projects.searchContents` with match case, whole word and regex, results grouped by file.

use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, HighlightStyle, IntoElement, ListAlignment, ListState, ParentElement, SharedString, Styled, StyledText, Task, div, list, prelude::*, px};
use t3_core::project_search::{
    ContentOptions, ContentsResult, MatchGroup, SEARCH_CONTENTS, SEARCH_DEBOUNCE_MS, SEARCH_QUERY_MAX_LENGTH, content_query_too_long,
    contents_request, group_matches, results_summary, split_path, utf16_byte_ranges,
};

use super::CommandPalette;
use super::files::{NO_FILE_READ_ACCESS, grants_filesystem_read};
use crate::app_state::AppState;
use crate::project_files::ProjectTarget;
use crate::theme::{hsla, palette};

enum Row {
    Header(usize),
    Match(usize),
}

pub(super) struct ContentSearch {
    pub target: Option<ProjectTarget>,
    pub options: ContentOptions,
    query: String,
    result: Option<ContentsResult>,
    groups: Vec<MatchGroup>,
    rows: Vec<Row>,
    pending: bool,
    pub error: Option<String>,
    pub selected: usize,
    list: ListState,
    request: Option<Task<()>>,
}

impl Default for ContentSearch {
    fn default() -> Self {
        ContentSearch {
            target: None,
            options: ContentOptions::default(),
            query: String::new(),
            result: None,
            groups: Vec::new(),
            rows: Vec::new(),
            pending: false,
            error: None,
            selected: 0,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            request: None,
        }
    }
}

impl ContentSearch {
    pub fn start(&mut self, target: Option<ProjectTarget>, state: &AppState) {
        let missing_access = target.is_some() && !grants_filesystem_read(state);
        *self = ContentSearch {
            target,
            error: missing_access.then(|| NO_FILE_READ_ACCESS.to_owned()),
            ..Default::default()
        };
    }

    pub fn search(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        self.query = query;
        let Some(target) = self.target.clone() else {
            self.request = None;
            self.pending = false;
            self.error = None;
            self.set_result(None);
            return;
        };
        if !grants_filesystem_read(state.read(cx)) {
            self.request = None;
            self.pending = false;
            self.error = Some(NO_FILE_READ_ACCESS.to_owned());
            self.set_result(None);
            return;
        }
        if content_query_too_long(&self.query) {
            self.request = None;
            self.pending = false;
            self.error = Some(format!("Search is limited to {SEARCH_QUERY_MAX_LENGTH} characters."));
            self.set_result(None);
            return;
        }
        let Some(payload) = contents_request(&target.cwd, &self.query, self.options) else {
            self.request = None;
            self.pending = false;
            self.error = None;
            self.set_result(None);
            return;
        };
        self.pending = true;
        self.error = None;
        let state = state.clone();
        self.request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
            let result = cx.update(|cx| state.read(cx).rpc(SEARCH_CONTENTS, payload)).await;
            let _ = this.update(cx, |palette, cx| {
                let content = &mut palette.content;
                content.pending = false;
                match result.map(serde_json::from_value::<ContentsResult>) {
                    Ok(Ok(result)) => {
                        content.error = None;
                        content.set_result(Some(result));
                    }
                    Ok(Err(error)) => content.error = Some(error.to_string()),
                    Err(error) => content.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    pub fn toggle_option(&mut self, pick: fn(&mut ContentOptions), state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        if !grants_filesystem_read(state.read(cx)) {
            return;
        }
        pick(&mut self.options);
        let query = self.query.clone();
        self.search(query, state, cx);
        cx.notify();
    }

    fn set_result(&mut self, result: Option<ContentsResult>) {
        self.groups = result.as_ref().map(|result| group_matches(&result.matches)).unwrap_or_default();
        self.rows = self.groups.iter().enumerate().flat_map(|(group, found)| std::iter::once(Row::Header(group)).chain(found.matches.iter().map(|m| Row::Match(*m)))).collect();
        self.result = result;
        self.selected = 0;
        self.list.reset(self.rows.len());
    }

    fn matches_len(&self) -> usize {
        self.result.as_ref().map_or(0, |result| result.matches.len())
    }

    pub fn move_selection(&mut self, step: isize) {
        let count = self.matches_len() as isize;
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + step).rem_euclid(count) as usize;
        if let Some(row) = self.rows.iter().position(|row| matches!(row, Row::Match(index) if *index == self.selected)) {
            self.list.scroll_to_reveal_item(row);
        }
    }

    /// The selected match's path and line, to open.
    pub fn selected_match(&self) -> Option<(String, u64)> {
        if self.pending || self.error.is_some() {
            return None;
        }
        let found = self.result.as_ref()?.matches.get(self.selected)?;
        Some((found.path.clone(), found.line_number))
    }

    fn invalid_regex(&self) -> bool {
        self.options.use_regex && self.result.as_ref().is_some_and(|result| result.regex_fallback_error.is_some())
    }

    pub fn render(&self, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        if self.target.is_none() {
            return div().m_auto().px(px(24.)).text_sm().text_color(hsla(p.muted_foreground)).child("Open a project to search its files.").into_any_element();
        }
        let has_query = !self.query.trim().is_empty();
        let status: Option<AnyElement> = if self.pending {
            Some(div().child("Searching…").into_any_element())
        } else if let Some(error) = &self.error {
            Some(div().text_color(hsla(p.destructive)).child(error.clone()).into_any_element())
        } else if self.invalid_regex() {
            Some(div().text_color(hsla(p.destructive)).child("Invalid regular expression").into_any_element())
        } else {
            self.result.as_ref().filter(|_| has_query).map(|result| div().child(results_summary(result)).into_any_element())
        };
        let body = if self.rows.is_empty() {
            let (color, message) = if let Some(error) = &self.error {
                (p.destructive, error.as_str())
            } else if has_query && !self.pending {
                (p.muted_foreground, "No results found.")
            } else {
                (p.muted_foreground, "Type to search across your project.")
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_sm()
                .text_color(hsla(color))
                .child(message.to_owned())
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|this, index: usize, _, cx| this.content.render_row(index, cx))).flex_1().py(px(8.)).into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(status.map(|status| div().h(px(36.)).flex_none().flex().items_center().px(px(12.)).border_b_1().border_color(hsla(p.border)).text_xs().text_color(hsla(p.muted_foreground)).child(status)))
            .child(body)
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let Some(result) = &self.result else { return div().into_any_element() };
        match self.rows.get(index) {
            Some(Row::Header(group)) => {
                let group = &self.groups[*group];
                let (name, directory) = split_path(&group.path);
                div()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .text_xs()
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(name.to_owned()))
                    .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(directory.to_owned()))
                    .child(div().ml_auto().px(px(6.)).rounded_full().bg(hsla(p.muted)).text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(group.matches.len().to_string()))
                    .into_any_element()
            }
            Some(Row::Match(found_index)) => {
                let found_index = *found_index;
                let found = &result.matches[found_index];
                let selected = found_index == self.selected;
                let mark = HighlightStyle { background_color: Some(hsla(p.search_match_background)), color: Some(hsla(p.search_match_foreground)), ..Default::default() };
                let line = StyledText::new(SharedString::from(found.line_content.clone()))
                    .with_highlights(utf16_byte_ranges(&found.line_content, &found.match_ranges).into_iter().map(move |range| (range, mark)));
                div()
                    .id(("content-match", found_index))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(12.))
                    .font_family("SF Mono")
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                    .when(!selected, |this| this.hover(|this| this.bg(hsla(p.accent).opacity(0.6))))
                    .child(div().w(px(40.)).flex_none().flex().justify_end().text_color(hsla(p.muted_foreground).opacity(0.7)).child(found.line_number.to_string()))
                    .child(div().flex_1().min_w_0().truncate().child(line))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.content.selected = found_index;
                        this.open_match(event.modifiers().platform, window, cx);
                    }))
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}
