//! Project content search mode (`apps/web/src/components/search/ProjectContentSearchDialog.tsx`
//! and `HighlightedSearchLine.tsx`): `projects.searchContents` with match case, whole word and
//! regex, results grouped by file with file icons and syntax-highlighted match lines.

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, HighlightStyle, Hsla, IntoElement, ListAlignment, ListState, ParentElement, SharedString, Styled, StyledText, Task, div, list, prelude::*, px};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::{ActiveTheme, Rope};
use t3_core::files::syntax_language;
use t3_core::project_search::{
    ContentOptions, ContentsResult, MatchGroup, MatchRange, SEARCH_CONTENTS, SEARCH_DEBOUNCE_MS, SEARCH_QUERY_MAX_LENGTH, content_query_too_long,
    contents_request, group_matches, results_summary, split_path, utf16_byte_ranges,
};

use super::CommandPalette;
use super::files::{NO_FILE_READ_ACCESS, file_icon, grants_filesystem_read};
use crate::app_state::AppState;
use crate::project_files::ProjectTarget;
use crate::theme::{hsla, palette};

/// Skip tree-sitter on giant minified lines so search result scrolling stays instant.
const MAX_SYNTAX_LINE_BYTES: usize = 4_096;
/// Bound per-result highlighted-line cache size.
const MAX_CACHED_LINES: usize = 256;

enum Row {
    Header(usize),
    Match(usize),
}

/// Merges sorted, non-overlapping syntax runs with sorted, non-overlapping match byte ranges so
/// every match keeps its syntax token style (`color`, `font_weight`, `font_style`, `underline`)
/// while receiving `match_bg` (`HighlightedSearchLine.tsx`: `bg-primary/25 text-inherit`).
pub(super) fn merge_syntax_and_match_runs(
    line_len: usize,
    syntax_runs: &[(Range<usize>, HighlightStyle)],
    match_ranges: &[Range<usize>],
    match_bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if line_len == 0 || (syntax_runs.is_empty() && match_ranges.is_empty()) {
        return Vec::new();
    }
    let mut points = Vec::with_capacity(syntax_runs.len() * 2 + match_ranges.len() * 2);
    for (range, _) in syntax_runs {
        let start = range.start.min(line_len);
        let end = range.end.min(line_len);
        if start < end {
            points.push(start);
            points.push(end);
        }
    }
    for range in match_ranges {
        let start = range.start.min(line_len);
        let end = range.end.min(line_len);
        if start < end {
            points.push(start);
            points.push(end);
        }
    }
    points.sort_unstable();
    points.dedup();
    if points.len() < 2 {
        return Vec::new();
    }
    let mut s_idx = 0usize;
    let mut m_idx = 0usize;
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(points.len());
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        while s_idx < syntax_runs.len() && syntax_runs[s_idx].0.end <= a {
            s_idx += 1;
        }
        while m_idx < match_ranges.len() && match_ranges[m_idx].end <= a {
            m_idx += 1;
        }
        let syntax = syntax_runs
            .get(s_idx)
            .filter(|(range, _)| range.start <= a && range.end >= b)
            .map(|(_, style)| *style);
        let is_match = match_ranges.get(m_idx).is_some_and(|range| range.start <= a && range.end >= b);
        let style = match (syntax, is_match) {
            (Some(mut style), true) => {
                style.background_color = Some(match_bg);
                style
            }
            (Some(style), false) => style,
            (None, true) => HighlightStyle { background_color: Some(match_bg), ..Default::default() },
            (None, false) => continue,
        };
        match out.last_mut() {
            Some((last_range, last_style)) if last_range.end == a && *last_style == style => last_range.end = b,
            _ => out.push((a..b, style)),
        }
    }
    out
}

pub(super) fn highlight_search_line(
    path: &str,
    line: &str,
    match_ranges: &[MatchRange],
    theme: &HighlightTheme,
    match_bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let matches = utf16_byte_ranges(line, match_ranges);
    let syntax_runs: Vec<(Range<usize>, HighlightStyle)> = if !line.is_empty() && line.len() <= MAX_SYNTAX_LINE_BYTES {
        syntax_language(path)
            .map(|language| {
                let mut highlighter = SyntaxHighlighter::new(language);
                highlighter.update(None, &Rope::from_str(line), None);
                highlighter
                    .styles(&(0..line.len()), theme)
                    .into_iter()
                    .filter(|(range, style)| range.start < range.end && *style != HighlightStyle::default())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    merge_syntax_and_match_runs(line.len(), &syntax_runs, &matches, match_bg)
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
    line_cache: HashMap<usize, Vec<(Range<usize>, HighlightStyle)>>,
    cache_theme_key: Option<(bool, u32)>,
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
            line_cache: HashMap::new(),
            cache_theme_key: None,
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
        let conn_id = state.read(cx).connection_id();
        self.pending = true;
        self.error = None;
        let state = state.clone();
        self.request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
            enum Preflight {
                StaleConnection,
                NoAccess,
                Send,
            }
            let preflight = state.read_with(cx, |app, _| {
                if app.connection_id() != conn_id {
                    Preflight::StaleConnection
                } else if !grants_filesystem_read(app) {
                    Preflight::NoAccess
                } else {
                    Preflight::Send
                }
            });
            match preflight {
                Preflight::Send => {}
                Preflight::NoAccess => {
                    let _ = this.update(cx, |palette, cx| {
                        let content = &mut palette.content;
                        content.pending = false;
                        content.set_result(None);
                        content.error = Some(NO_FILE_READ_ACCESS.to_owned());
                        cx.notify();
                    });
                    return;
                }
                Preflight::StaleConnection => {
                    let _ = this.update(cx, |palette, cx| {
                        palette.content.pending = false;
                        cx.notify();
                    });
                    return;
                }
            }
            let result = cx.update(|cx| state.read(cx).rpc(SEARCH_CONTENTS, payload)).await;
            let _ = this.update(cx, |palette, cx| {
                let app = state.read(cx);
                let content = &mut palette.content;
                content.pending = false;
                if app.connection_id() != conn_id {
                    cx.notify();
                    return;
                }
                if !grants_filesystem_read(app) {
                    content.set_result(None);
                    content.error = Some(NO_FILE_READ_ACCESS.to_owned());
                    cx.notify();
                    return;
                }
                match result.map(serde_json::from_value::<ContentsResult>) {
                    Ok(Ok(result)) => {
                        content.error = None;
                        content.set_result(Some(result));
                    }
                    Ok(Err(error)) => {
                        content.set_result(None);
                        content.error = Some(error.to_string());
                    }
                    Err(error) => {
                        content.set_result(None);
                        content.error = Some(error.to_string());
                    }
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
        self.line_cache.clear();
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
        let can_read = grants_filesystem_read(cx.entity().read(cx).state.read(cx));
        let has_query = !self.query.trim().is_empty();
        let status: Option<AnyElement> = if !can_read {
            None
        } else if self.pending {
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
                (if can_read { p.destructive } else { p.muted_foreground }, error.as_str())
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

    fn render_row(&mut self, index: usize, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let theme_key = (cx.theme().mode.is_dark(), p.primary);
        if self.cache_theme_key != Some(theme_key) {
            self.cache_theme_key = Some(theme_key);
            self.line_cache.clear();
        }
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
                    .child(file_icon(&group.path).render(px(14.), hsla(p.muted_foreground).opacity(0.8)))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(name.to_owned()))
                    .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(directory.to_owned()))
                    .child(div().ml_auto().px(px(6.)).rounded_full().bg(hsla(p.muted)).text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(group.matches.len().to_string()))
                    .into_any_element()
            }
            Some(Row::Match(found_index)) => {
                let found_index = *found_index;
                let found = &result.matches[found_index];
                let selected = found_index == self.selected;
                let runs = if let Some(cached) = self.line_cache.get(&found_index) {
                    cached.clone()
                } else {
                    if self.line_cache.len() >= MAX_CACHED_LINES {
                        self.line_cache.clear();
                    }
                    let highlight_theme = cx.theme().highlight_theme.clone();
                    let match_bg = hsla(p.primary).opacity(0.25);
                    let computed = highlight_search_line(&found.path, &found.line_content, &found.match_ranges, &highlight_theme, match_bg);
                    self.line_cache.insert(found_index, computed.clone());
                    computed
                };
                let line = StyledText::new(SharedString::from(found.line_content.clone())).with_highlights(runs);
                div()
                    .id(("content-match", found_index))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(12.))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                    .when(!selected, |this| this.hover(|this| this.bg(hsla(p.accent).opacity(0.6))))
                    .child(div().w(px(40.)).flex_none().flex().justify_end().text_color(hsla(p.muted_foreground).opacity(0.7)).child(found.line_number.to_string()))
                    .child(div().flex_1().min_w_0().truncate().child(line))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.content.selected != found_index {
                            this.content.selected = found_index;
                            cx.notify();
                        }
                    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_syntax_and_match_runs_preserves_syntax_inside_matches_without_overlaps() {
        let kw = HighlightStyle { color: Some(gpui::red()), font_weight: Some(FontWeight::BOLD), ..Default::default() };
        let ident = HighlightStyle { color: Some(gpui::blue()), ..Default::default() };
        let bg = gpui::green();
        // "fn hello_world()"
        let syntax = vec![(0..2, kw), (3..14, ident)];
        // Match spans across the space and partway into `hello_world`: 1..8
        let merged = merge_syntax_and_match_runs(16, &syntax, std::slice::from_ref(&(1..8)), bg);
        assert_eq!(
            merged,
            vec![
                (0..1, kw),
                (1..2, HighlightStyle { background_color: Some(bg), ..kw }),
                (2..3, HighlightStyle { background_color: Some(bg), ..Default::default() }),
                (3..8, HighlightStyle { background_color: Some(bg), ..ident }),
                (8..14, ident),
            ]
        );
        // Verify strictly sorted, non-overlapping invariant.
        for pair in merged.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start);
        }
    }

    #[test]
    fn highlight_search_line_syntax_highlights_rust_and_marks_utf16_matches() {
        let theme = crate::theme::test_syntax_theme(true);
        let bg = gpui::yellow();
        let line = "pub fn greet() -> &'static str { \"hi\" }";
        // "greet" is at UTF-16 offset 7..12.
        let runs = highlight_search_line("src/lib.rs", line, &[MatchRange { start: 7, end: 12 }], &theme, bg);
        assert!(!runs.is_empty(), "expected syntax and match runs");
        assert!(runs.iter().any(|(range, style)| range.start <= 7 && range.end >= 12 && style.background_color == Some(bg)));
        // Non-match keyword `pub`/`fn` still gets syntax styling without match background.
        assert!(runs.iter().any(|(range, style)| range.end <= 6 && style.color.is_some() && style.background_color.is_none()));
        for pair in runs.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start);
        }
    }
}
