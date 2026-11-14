//! `orchestration.getThreadTitles` lookup (`#283`) for related threads whose shell rows are not
//! loaded: keeps unlisted archived/omitted threads reachable with fresh titles and prunes deleted
//! forks once a settled response omits them.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Entity, Task};
use t3_client::supervisor::ConnectionStatus;
use t3_core::lineage::{GET_THREAD_TITLES, decode_thread_titles, thread_titles_request};

use crate::app_state::{AppState, Phase};

const MAX_AUTO_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupKey {
    connection: u64,
    ids: Vec<String>,
    removed_at: Vec<Option<u64>>,
}

impl LookupKey {
    fn build(state: &AppState, connection: u64, ids: Vec<String>) -> Self {
        let removed_at = ids.iter().map(|id| state.shell.removed_at.get(id).cloned()).collect();
        Self { connection, ids, removed_at }
    }
}

#[derive(Default)]
pub struct ThreadTitlesFeed {
    connection: Option<u64>,
    active_key: Option<LookupKey>,
    settled_key: Option<LookupKey>,
    failed_key: Option<LookupKey>,
    retry_attempt: u32,
    titles: HashMap<String, String>,
    in_flight: Option<Task<()>>,
    retry_task: Option<Task<()>>,
}

impl ThreadTitlesFeed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_retry(&mut self) {
        self.failed_key = None;
        self.retry_task = None;
    }

    fn clear_pending(&mut self) {
        self.active_key = None;
        self.failed_key = None;
        self.retry_attempt = 0;
        self.in_flight = None;
        self.retry_task = None;
    }

    /// Returns settled titles only when they match the current `(connection, unlisted_ids, removed_at)`.
    pub(super) fn resolved_for(&self, state: &AppState, unlisted_ids: &[String]) -> Option<&HashMap<String, String>> {
        if unlisted_ids.is_empty() {
            return None;
        }
        let connection = state.connection_id()?;
        let key = LookupKey::build(state, connection, unlisted_ids.to_vec());
        (self.settled_key.as_ref() == Some(&key)).then_some(&self.titles)
    }

    pub fn sync<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        active: bool,
        feed_mut: fn(&mut V) -> &mut Self,
        on_settled: fn(&mut V, &mut Context<V>),
        cx: &mut Context<V>,
    ) {
        let app = state.read(cx);
        let online = matches!(app.phase, Phase::Online(ConnectionStatus::Connected));
        let connection = app.connection_id();
        if self.connection != connection {
            self.connection = connection;
            self.settled_key = None;
            self.titles.clear();
            self.clear_pending();
        }
        let Some(connection) = connection.filter(|_| active && online && app.can_read_threads()) else {
            self.clear_pending();
            return;
        };
        let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) else {
            self.clear_pending();
            return;
        };
        let ids = super::relationships::unlisted_ids(app, thread);
        if ids.is_empty() {
            self.settled_key = None;
            self.titles.clear();
            self.clear_pending();
            return;
        }
        let key = LookupKey::build(app, connection, ids);
        if self.active_key.as_ref() == Some(&key)
            || self.settled_key.as_ref() == Some(&key)
            || self.failed_key.as_ref() == Some(&key)
        {
            return;
        }
        if self.failed_key.is_some() {
            self.retry_attempt = 0;
            self.failed_key = None;
            self.retry_task = None;
        }
        let request = app.rpc(GET_THREAD_TITLES, thread_titles_request(&key.ids));
        let state_entity = state.clone();
        self.active_key = Some(key.clone());
        self.in_flight = Some(cx.spawn(async move |view, cx| {
            let titles = match request.await {
                Ok(value) => decode_thread_titles(value).ok(),
                Err(_) => None,
            };
            let _ = view.update(cx, |view, cx| {
                let feed = feed_mut(view);
                if feed.active_key.as_ref() != Some(&key) {
                    return;
                }
                feed.active_key = None;
                feed.in_flight = None;
                if let Some(titles) = titles {
                    feed.settled_key = Some(key);
                    feed.failed_key = None;
                    feed.retry_attempt = 0;
                    feed.retry_task = None;
                    feed.titles = titles;
                    on_settled(view, cx);
                    return;
                }
                feed.failed_key = Some(key.clone());
                if feed.retry_attempt >= MAX_AUTO_RETRIES {
                    return;
                }
                let attempt = feed.retry_attempt;
                feed.retry_attempt += 1;
                let delay = Duration::from_millis(500u64 << attempt.min(2));
                feed.retry_task = Some(cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = view.update(cx, |view, cx| {
                        let feed = feed_mut(view);
                        if feed.failed_key.as_ref() != Some(&key) {
                            return;
                        }
                        feed.failed_key = None;
                        feed.retry_task = None;
                        feed.sync(&state_entity, true, feed_mut, on_settled, cx);
                    });
                }));
            });
        }));
    }
}
