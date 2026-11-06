//! Sidebar thread search (`searchSidebarThreads` in `Sidebar.logic.ts`, the server half in
//! `client-runtime/src/state/threadSearch.ts`): title and branch matches ranked locally, then
//! threads listed only because their messages match, in the order the server ranked them. Below
//! the shell's older-settled cut the server also returns the best older rows, mounted while shown.

use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px, uniform_list};
use t3_core::sidebar::older_settled::{SEARCH_THREADS, search_request};
use t3_core::sidebar::tag_filter::ThreadKey;

use super::SidebarPane;
use crate::app_state::AppState;
use crate::theme::{hsla, palette};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const RESULT_HEIGHT: f32 = 36.;

#[derive(Default)]
pub struct SearchState {
    query: String,
    results: Vec<ThreadKey>,
    content_matches: Vec<(ThreadKey, String)>,
    highlighted: usize,
    requests: Vec<Task<()>>,
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn highlighted_key(&self) -> Option<&ThreadKey> {
        self.results.get(self.highlighted)
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.results.len() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
        }
    }

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if query == self.query {
            return;
        }
        self.query = query;
        self.highlighted = 0;
        self.content_matches.clear();
        self.requests.clear();
        self.refresh(state, cx);
        let trimmed = self.query.trim().to_owned();
        let app = state.read(cx);
        let runtime = app.runtime().clone();
        let owners: Vec<_> = app.fleet.environments.iter().filter(|entry| app.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)).map(|entry| entry.summary.owner.clone()).collect();
        if owners.is_empty() {
            let generation = app.shell.generation;
            let payload = search_request(&trimmed, app.omitted.older().as_ref().map(|older| older.before.as_str()), None);
            let client = app.client_handle();
            state.update(cx, |state, cx| state.mount_search_rows(generation, None, cx));
            if let (Some(payload), Some(client)) = (payload, client) {
                let state = state.clone();
                self.requests.push(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                    let Ok(Ok(value)) = runtime.spawn(async move { client.request(SEARCH_THREADS, payload).await }).await else { return };
                    let _ = this.update(cx, |this, cx| {
                        if this.search.query.trim() != trimmed { return; }
                        this.search.content_matches = decode_matches(&value, "");
                        state.update(cx, |state, cx| state.mount_search_rows(generation, serde_json::from_value(value).ok(), cx));
                        this.search.refresh(&state, cx); cx.notify();
                    });
                }));
            }
            return;
        }
        for owner in owners {
            let older = state.read(cx).sidebar.owner_omitted.get(&owner).and_then(|omitted| omitted.older());
            let payload = search_request(&trimmed, older.as_ref().map(|older| older.before.as_str()), None);
            state.update(cx, |state, cx| state.mount_scoped_search_rows(&owner, None, cx));
            let Some(payload) = payload else { continue };
            let resolve = state.read(cx).connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
            let state = state.clone(); let query = trimmed.clone(); let runtime = runtime.clone();
            self.requests.push(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                let Ok(target) = resolve.await else { return; };
                let Ok(Ok(value)) = runtime.spawn(async move { target.client.request(SEARCH_THREADS, payload).await }).await else { return };
                let _ = this.update(cx, |this, cx| {
                    if this.search.query.trim() != query || !state.read(cx).scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
                    this.search.content_matches.extend(decode_matches(&value, &owner.saved_id));
                    state.update(cx, |state, cx| state.mount_scoped_search_rows(&owner, serde_json::from_value(value).ok(), cx));
                    this.search.refresh(&state, cx); cx.notify();
                });
            }));
        }
    }

    /// Re-ranks against the current model, keeping the highlight on the same thread.
    pub fn refresh(&mut self, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if !self.is_active() {
            self.results.clear();
            return;
        }
        let previous = self.highlighted_key().cloned();
        let app = state.read(cx);
        let now = crate::sidebar::controller::now_ms();
        let mut results = if app.fleet.environments.is_empty() {
            t3_core::sidebar::search_titles(&app.sidebar.model, &self.query, now).into_iter().map(|id| ThreadKey::new("", &id)).collect()
        } else {
            use t3_core::search::{ThreadSearchOptions, ThreadSearchSubject, fold_search_query, rank_search_items, score_thread_search};
            let normalized = fold_search_query(&self.query);
            let subjects = app.sidebar.fleet.threads.iter().map(|row| {
                let thread = &row.thread;
                (row.key.clone(), ThreadSearchSubject { id: Some(thread.id.clone()), title: thread.title.clone(), branch: thread.branch.clone(),
                    project_title: app.sidebar.fleet.project_names.get(&row.project).cloned(),
                    activity_at: thread.latest_user_authored_message_at.unwrap_or_else(|| t3_core::sidebar::sort::thread_sort_timestamp(thread, t3_core::sidebar::sort::ThreadSortOrder::UpdatedAt)),
                    opens: thread.opens.clone(), ..Default::default() })
            }).collect::<Vec<_>>();
            rank_search_items(&subjects, |(_, subject), typos| score_thread_search(&normalized, subject, now, ThreadSearchOptions { typos, picks: false })).into_iter().map(|ranked| ranked.item.0.clone()).collect::<Vec<_>>()
        };
        for (key, _) in &self.content_matches {
            let exists = if key.environment_id.is_empty() { app.sidebar.model.thread(&key.thread_id).is_some() } else { app.sidebar.fleet.thread(key).is_some() };
            if exists && !results.contains(key) { results.push(key.clone()); }
        }
        self.highlighted = previous.and_then(|key| results.iter().position(|candidate| *candidate == key)).unwrap_or(0);
        self.results = results;
    }

    fn snippet(&self, thread_id: &ThreadKey) -> Option<&str> {
        self.content_matches.iter().find(|(id, _)| id == thread_id).map(|(_, snippet)| snippet.as_str()).filter(|s| !s.is_empty())
    }
}

impl SidebarPane {
    pub(super) fn render_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if self.search.results.is_empty() {
            return div().px(px(16.)).py(px(24.)).text_xs().text_color(hsla(p.muted_foreground).opacity(0.6)).child("No matching threads").into_any_element();
        }
        let count = self.search.results.len();
        uniform_list(
            "sidebar-search-results",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.render_search_result(index, cx)).collect::<Vec<_>>()),
        )
        .flex_1()
        .into_any_element()
    }

    fn render_search_result(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(id) = self.search.results.get(index).cloned() else { return div().into_any_element() };
        let state = self.state.read(cx);
        let scoped = state.sidebar.fleet.thread(&id);
        let Some(thread) = scoped.map(|row| &row.thread).or_else(|| state.sidebar.model.thread(&id.thread_id).filter(|_| id.environment_id.is_empty())) else { return div().h(px(RESULT_HEIGHT)).into_any_element() };
        let project = scoped.and_then(|row| state.sidebar.fleet.project_names.get(&row.project)).or_else(|| state.sidebar.model.project_names.get(&thread.project_id)).cloned();
        let highlighted = index == self.search.highlighted;
        let snippet = self.search.snippet(&id).map(|s| SharedString::from(s.to_owned()));
        let title: SharedString = thread.title.clone().into();
        div().h(px(RESULT_HEIGHT)).px(px(8.)).py(px(1.)).child(
            div()
                .id(SharedString::from(format!("search-result-{:?}", id)))
                .size_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(highlighted, |this| this.bg(hsla(p.sidebar_row_active)))
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                        .children(snippet.map(|snippet| div().truncate().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(snippet))),
                )
                .children(project.map(|name| div().flex_none().max_w(px(96.)).truncate().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(name)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.clear_search(window, cx);
                    this.state.update(cx, |state, cx| { if id.environment_id.is_empty() { state.select_thread(&id.thread_id, cx); }
                        else if let Some(owner) = state.scoped_environment(&id.environment_id).map(|entry| entry.summary.owner.clone()) { state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(id.thread_id.clone()), cx); } });
                })),
        )
        .into_any_element()
    }
}

fn decode_matches(value: &serde_json::Value, environment: &str) -> Vec<(ThreadKey, String)> {
    value.get("matches").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(|row| Some((ThreadKey::new(environment, row.get("threadId")?.as_str()?), row.get("snippet").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned()))).collect()
}
