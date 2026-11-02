//! Rows a shell subscribed with `omitOlderSettled` / `omitIdleSubagents` leaves to on-demand reads:
//! older settled pages for the Settled shelf and thread families, merged behind the shell's rows.
//! The state machines are `t3_core::sidebar::older_settled` and `t3_core::families`; this drives
//! their reads as `olderSettledThreads.ts` and `threadFamilies.ts` do.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Task};
use serde_json::Value;
use t3_core::families::{FamilyRead, GET_THREAD_FAMILY, MountedReads, family_key, family_read_due, family_request};
use t3_core::shell::ThreadShell;
use t3_core::sidebar::older_settled::{
    Cursor, GET_OLDER_SETTLED_THREADS, OlderSettled, OlderSettledCounts, PageStatus, SettledPages, count_request, page_request,
};

use crate::app_state::{AppEvent, AppState};

const PAGES_KEY: &str = "older-settled";
const SEARCH_KEY: &str = "search";
/// `COUNT_REREAD_DELAY`: a burst of changes (a PR sync touching many old threads) costs one read.
const COUNT_REREAD_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct OmittedRows {
    pub mounts: MountedReads,
    pub counts: OlderSettledCounts,
    pub pages: SettledPages,
    /// The count key last read (or being read), so a key is read once.
    count_key: Option<String>,
    count_task: Option<Task<()>>,
    /// The shell generation each family was last asked for, so a generation reads it once.
    family_requested: HashMap<String, u64>,
}

impl OmittedRows {
    /// The cut and newest counts, while the shell omits older settled threads.
    pub fn older(&self) -> Option<OlderSettled> {
        self.counts.current()
    }
}

impl AppState {
    /// A thread from the shell, else from a read the shell left rows to.
    pub fn thread_shell(&self, id: &str) -> Option<&ThreadShell> {
        self.omitted.mounts.thread(&self.shell, id)
    }

    /// Follows a shell change: counts, pages, and family reads for the new generation.
    pub(crate) fn sync_omitted_rows(&mut self, cx: &mut Context<Self>) {
        let generation = self.shell.generation;
        if let Some(key) = self.omitted.counts.sync(&self.shell)
            && self.omitted.count_key.as_deref() != Some(key.as_str())
        {
            self.read_counts(key, cx);
        }
        let older = self.omitted.older();
        self.omitted.pages.sync(&self.shell, older.as_ref());
        if self.omitted.pages.reads.is_empty() {
            self.omitted.mounts.unmount(PAGES_KEY);
        }
        self.request_settled_page(cx);
        // A new snapshot leaves earlier family reads out; read the ones still mounted again.
        let stale: Vec<String> = self
            .omitted
            .mounts
            .keys()
            .filter_map(|key| key.strip_prefix("family:"))
            .map(str::to_owned)
            .collect();
        for thread_id in stale {
            if self.omitted.mounts.generation_of(&family_key(&thread_id)) != Some(generation)
                && self.omitted.family_requested.get(&thread_id) != Some(&generation)
            {
                self.read_family(&thread_id, false, cx);
            }
        }
        self.mount_selected_family(cx);
    }

    /// Keeps the open thread's family mounted while the shell itself leaves the thread out, so
    /// a snapshot that cuts it, or a search or page that lets go of it, never hides it.
    pub(crate) fn mount_selected_family(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        let requested = self.omitted.family_requested.get(&thread_id).copied();
        if family_read_due(&self.shell, &self.omitted.mounts, &thread_id, requested) {
            self.read_family(&thread_id, false, cx);
        }
    }

    /// One more older page while the expanded shelf lists fewer rows than it shows.
    pub(crate) fn grow_settled_shelf(&mut self, listed: usize, cx: &mut Context<Self>) {
        let visible = if self.sidebar.shelves.settled_expanded { self.sidebar.shelves.settled_visible } else { 0 };
        let older = self.omitted.older();
        if self.omitted.pages.grow(older.as_ref(), listed, visible) {
            self.request_settled_page(cx);
        }
    }

    /// Reads a failed page again; the pages after it follow.
    pub fn retry_settled_pages(&mut self, cx: &mut Context<Self>) {
        self.omitted.pages.retry();
        self.request_settled_page(cx);
    }

    /// An older page is on its way; the Settled shelf's button waits for it.
    pub fn settled_shelf_loading(&self) -> bool {
        self.omitted.pages.status == PageStatus::Pending
    }

    /// The Settled shelf's "Show more" button, as the web words it.
    pub fn settled_show_more_label(&self, count: usize) -> String {
        match self.omitted.pages.status {
            PageStatus::Pending => "Loading".into(),
            PageStatus::Failed => "Could not load older threads. Retry".into(),
            PageStatus::Ready => format!("Show {count} more"),
        }
    }

    fn request_settled_page(&mut self, cx: &mut Context<Self>) {
        let Some((generation, before)) = self.omitted.pages.next_request() else { return };
        let rpc = self.rpc(GET_OLDER_SETTLED_THREADS, page_request(&before, None));
        cx.spawn(async move |this, cx| {
            let page = rpc.await.map_err(|error| error.to_string()).and_then(decode_read);
            let _ = this.update(cx, |state, cx| state.apply_settled_page(generation, &before, page, cx));
        })
        .detach();
        // Only the shelf's button changes; the rows wait for the page.
        cx.emit(AppEvent::Notice);
    }

    fn apply_settled_page(&mut self, generation: u64, before: &Cursor, page: Result<FamilyRead, String>, cx: &mut Context<Self>) {
        if !self.omitted.pages.apply_page(generation, before, page) {
            return;
        }
        self.omitted.mounts.mount(PAGES_KEY, generation, self.omitted.pages.reads.clone());
        self.rebuild_sidebar(cx);
        self.sync_side_threads(cx);
        self.request_settled_page(cx);
        cx.emit(AppEvent::Shell);
    }

    fn read_counts(&mut self, key: String, cx: &mut Context<Self>) {
        self.omitted.count_key = Some(key.clone());
        self.omitted.count_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COUNT_REREAD_DELAY).await;
            let _ = this.update(cx, |state, cx| state.send_count_read(key, cx));
        }));
    }

    fn send_count_read(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(older) = self.omitted.older() else { return };
        let rpc = self.rpc(GET_OLDER_SETTLED_THREADS, count_request(&older.before));
        self.omitted.count_task = Some(cx.spawn(async move |this, cx| {
            let Ok(value) = rpc.await else { return };
            let counts = parse_counts(&value);
            let _ = this.update(cx, |state, cx| {
                state.omitted.counts.apply_read(&key, counts);
                state.rebuild_sidebar(cx);
                cx.emit(AppEvent::Shell);
            });
        }));
    }

    /// Reads the family of a thread the shell omits and merges it. With `select`, selects the
    /// thread once the family proves it exists (a restored thread that is now older or idle).
    pub(crate) fn read_family(&mut self, thread_id: &str, select: bool, cx: &mut Context<Self>) {
        let families = self.server_config.config.as_ref().is_some_and(|config| config.shell_thread_families);
        if !families {
            return;
        }
        let generation = self.shell.generation;
        self.omitted.family_requested.insert(thread_id.to_owned(), generation);
        let rpc = self.rpc(GET_THREAD_FAMILY, family_request(thread_id));
        let thread_id = thread_id.to_owned();
        cx.spawn(async move |this, cx| {
            let Ok(family) = rpc.await.map_err(|error| error.to_string()).and_then(decode_read) else { return };
            let _ = this.update(cx, |state, cx| {
                if state.shell.generation != generation {
                    return;
                }
                let exists = family.threads.iter().any(|thread| thread.id == thread_id);
                state.omitted.mounts.mount(&family_key(&thread_id), generation, vec![family]);
                if select && exists && state.selected_thread.is_none() && state.draft_project.is_none() {
                    state.select_thread(&thread_id, cx);
                }
                state.rebuild_sidebar(cx);
                state.sync_side_threads(cx);
                cx.emit(AppEvent::Shell);
            });
        })
        .detach();
    }

    /// Mounts a search's older settled rows until newer results replace them, or the search ends
    /// (`None`), so the sidebar's own ranking finds them as it finds shell rows (`useThreadSearch`).
    pub(crate) fn mount_search_rows(&mut self, generation: u64, read: Option<FamilyRead>, cx: &mut Context<Self>) {
        match read.filter(|read| !read.threads.is_empty()) {
            Some(read) => self.omitted.mounts.mount(SEARCH_KEY, generation, vec![read]),
            None if self.omitted.mounts.generation_of(SEARCH_KEY).is_none() => return,
            None => self.omitted.mounts.unmount(SEARCH_KEY),
        }
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    /// Families mount while their thread is open; switching threads lets them go.
    pub(crate) fn release_families(&mut self, keep: Option<&str>) {
        self.omitted.family_requested.retain(|thread_id, _| Some(thread_id.as_str()) == keep);
        let keep = keep.map(family_key);
        let stale: Vec<String> =
            self.omitted.mounts.keys().filter(|key| key.starts_with("family:") && Some(*key) != keep.as_deref()).map(str::to_owned).collect();
        for key in stale {
            self.omitted.mounts.unmount(&key);
        }
    }
}

fn decode_read(value: Value) -> Result<FamilyRead, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn parse_counts(value: &Value) -> HashMap<String, u64> {
    value
        .get("counts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry.get("projectId")?.as_str()?.to_owned(), entry.get("count")?.as_u64()?)))
        .collect()
}
