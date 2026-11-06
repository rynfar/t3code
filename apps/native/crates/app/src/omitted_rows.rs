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
    scoped_projects: Option<Vec<String>>,
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
        if !self.fleet.environments.is_empty() {
            let owners = self.sidebar.owner_omitted.iter_mut().map(|(owner, omitted)| { omitted.pages.retry(); owner.clone() }).collect::<Vec<_>>();
            for owner in owners { self.request_scoped_settled_page(owner, cx); }
            return;
        }
        self.omitted.pages.retry();
        self.request_settled_page(cx);
    }

    /// An older page is on its way; the Settled shelf's button waits for it.
    pub fn settled_shelf_loading(&self) -> bool {
        self.omitted.pages.status == PageStatus::Pending || self.sidebar.owner_omitted.values().any(|omitted| omitted.pages.status == PageStatus::Pending)
    }

    /// The Settled shelf's "Show more" button, as the web words it.
    pub fn settled_show_more_label(&self, count: usize) -> String {
        let status = if self.sidebar.owner_omitted.values().any(|omitted| omitted.pages.status == PageStatus::Pending) { PageStatus::Pending }
            else if self.sidebar.owner_omitted.values().any(|omitted| omitted.pages.status == PageStatus::Failed) { PageStatus::Failed } else { self.omitted.pages.status };
        match status {
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
        let connection = self.connection_id();
        self.omitted.family_requested.insert(thread_id.to_owned(), generation);
        self.restoring_thread |= select;
        let rpc = self.rpc(GET_THREAD_FAMILY, family_request(thread_id));
        let thread_id = thread_id.to_owned();
        cx.spawn(async move |this, cx| {
            let read = rpc.await.map_err(|error| error.to_string()).and_then(decode_read);
            let _ = this.update(cx, |state, cx| {
                if state.connection_id() != connection { return; }
                if let Ok(family) = read
                    && state.shell.generation == generation
                {
                    let exists = family.threads.iter().any(|thread| thread.id == thread_id);
                    state.omitted.mounts.mount(&family_key(&thread_id), generation, vec![family]);
                    state.rebuild_sidebar(cx);
                    if select && exists && state.selected_thread.is_none() && state.draft_project.is_none() {
                        state.select_thread(&thread_id, cx);
                    }
                    state.sync_side_threads(cx);
                    cx.emit(AppEvent::Shell);
                }
                // A restore that finds nothing falls back to the home page.
                if select {
                    state.restoring_thread = false;
                    state.land_home(cx);
                }
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

impl AppState {
    pub(crate) fn sync_scoped_omitted_rows(&mut self, cx: &mut Context<Self>) {
        let entries = self.fleet.environments.clone();
        for entry in entries {
            let summary = &entry.summary;
            let owner = summary.owner.clone();
            if !self.sidebar.owner_omitted.contains_key(&owner) {
                let previous = self.sidebar.owner_omitted.keys().find(|previous| previous.saved_id==owner.saved_id && previous.canonical_id==owner.canonical_id && previous.generation==owner.generation && previous.connection_generation==owner.connection_generation).cloned();
                if let Some(previous)=previous && let Some(mut omitted)=self.sidebar.owner_omitted.remove(&previous) {
                    omitted.count_task=None; omitted.count_key=None; omitted.family_requested.clear();
                    self.sidebar.owner_omitted.insert(owner.clone(),omitted);
                }
            }
            let omitted = self.sidebar.owner_omitted.entry(owner.clone()).or_default();
            let count = omitted.counts.sync_snapshot(owner.shell_generation, summary.snapshot_sequence, summary.older_settled.as_ref(), summary.older_count_key.as_deref());
            let older = omitted.older(); omitted.pages.sync_generation(owner.shell_generation, older.as_ref());
            if omitted.pages.reads.is_empty() { omitted.mounts.unmount(PAGES_KEY); }
            let families=omitted.mounts.keys().filter_map(|key| key.strip_prefix("family:")).map(str::to_owned).collect::<Vec<_>>();
            let count=count.filter(|key| omitted.count_key.as_ref()!=Some(key));
            if let Some(key)=count { self.sidebar.owner_omitted.get_mut(&owner).unwrap().count_key=Some(key.clone()); self.read_scoped_counts(owner.clone(),key,cx); }
            for id in families { self.read_scoped_family(&owner,&id,cx); }
            self.request_scoped_settled_page(owner, cx);
        }
    }

    pub(crate) fn read_scoped_family(&mut self, owner:&t3_core::sidebar::grouping::EnvironmentOwner, id:&str, cx:&mut Context<Self>) {
        if !self.scoped_ready(owner,t3_core::auth::ORCHESTRATION_READ_SCOPE) || !self.scoped_environment(&owner.saved_id).and_then(|entry| entry.config.config.as_ref()).is_some_and(|config| config.shell_thread_families) { return; }
        let omitted=self.sidebar.owner_omitted.entry(owner.clone()).or_default();
        if omitted.mounts.generation_of(&family_key(id))==Some(owner.shell_generation) || omitted.family_requested.get(id)==Some(&owner.shell_generation) { return; }
        omitted.family_requested.insert(id.into(),owner.shell_generation);
        let resolve=self.connections.destination(owner,t3_core::auth::ORCHESTRATION_READ_SCOPE);
        let runtime=self.runtime().clone(); let owner=owner.clone(); let id=id.to_owned();
        cx.spawn(async move |this,cx| {
            let Ok(target)=resolve.await else { return };
            if !this.read_with(cx,|state,_| target.is_current(&state.fleet) && state.scoped_ready(&owner,t3_core::auth::ORCHESTRATION_READ_SCOPE)
                && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE))).unwrap_or(false) { return; }
            let requested=id.clone();
            let Ok(Ok(value))=runtime.spawn(async move { target.client.request(GET_THREAD_FAMILY,family_request(&requested)).await }).await else { return };
            let Ok(read)=decode_read(value) else { return };
            let _=this.update(cx,|state,cx| {
                if !state.scoped_ready(&owner,t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
                state.sidebar.owner_omitted.entry(owner.clone()).or_default().mounts.mount(&family_key(&id),owner.shell_generation,vec![read]); state.rebuild_sidebar(cx); cx.emit(AppEvent::Shell);
            });
        }).detach();
    }

    fn read_scoped_counts(&mut self, owner: t3_core::sidebar::grouping::EnvironmentOwner, key: String, cx: &mut Context<Self>) {
        let Some(older) = self.sidebar.owner_omitted.get(&owner).and_then(|omitted| omitted.older()) else { return };
        let resolve = self.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
        let runtime = self.runtime().clone();
        let task_owner = owner.clone();
        let task = cx.spawn(async move |this, cx| {
            let owner = task_owner;
            cx.background_executor().timer(COUNT_REREAD_DELAY).await;
            let Ok(target) = resolve.await else { return };
            let Ok(Ok(value)) = runtime.spawn(async move { target.client.request(GET_OLDER_SETTLED_THREADS, count_request(&older.before)).await }).await else { return };
            let _ = this.update(cx, |state, cx| {
                if !state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
                state.sidebar.owner_omitted.entry(owner.clone()).or_default().counts.apply_read(&key, parse_counts(&value));
                state.rebuild_sidebar(cx); cx.emit(AppEvent::Shell);
            });
        });
        self.sidebar.owner_omitted.entry(owner).or_default().count_task = Some(task);
    }

    fn scoped_older_projects(&self, owner: &t3_core::sidebar::grouping::EnvironmentOwner, cx:&gpui::App) -> Option<Vec<String>> {
        let groups = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar,cx);
        let scope = self.project_preferences.scoped_members(&groups,self.fleet.environments.iter().all(|entry| entry.summary.synchronized))?;
        let mut ids = scope.into_iter().filter(|reference| reference.environment_id == owner.saved_id).map(|reference| reference.project_id).collect::<Vec<_>>(); ids.sort(); Some(ids)
    }

    pub(crate) fn grow_scoped_settled_shelf(&mut self, listed: usize, cx: &mut Context<Self>) {
        let visible = if self.sidebar.shelves.settled_expanded { self.sidebar.shelves.settled_visible } else { 0 };
        if listed >= visible { return; }
        let scopes = self.sidebar.owner_omitted.keys().map(|owner| (owner.clone(),self.scoped_older_projects(owner,cx))).collect::<Vec<_>>();
        let mut owners = Vec::new();
        for (owner,scope) in scopes {
            if scope.as_ref().is_some_and(Vec::is_empty) { continue; }
            let omitted = self.sidebar.owner_omitted.get_mut(&owner).unwrap();
            if omitted.scoped_projects != scope { omitted.pages = SettledPages::default(); omitted.scoped_projects = scope; let older=omitted.older(); omitted.pages.sync_generation(owner.shell_generation,older.as_ref()); omitted.mounts.unmount(PAGES_KEY); }
            let older = omitted.older();
            if omitted.pages.grow(older.as_ref(),listed,visible) { owners.push(owner); }
        }
        for owner in owners { self.request_scoped_settled_page(owner, cx); }
    }

    fn request_scoped_settled_page(&mut self, owner: t3_core::sidebar::grouping::EnvironmentOwner, cx: &mut Context<Self>) {
        if !self.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
        let project_ids = self.scoped_older_projects(&owner,cx);
        if project_ids.as_ref().is_some_and(Vec::is_empty) { return; }
        let omitted = self.sidebar.owner_omitted.entry(owner.clone()).or_default();
        if omitted.scoped_projects != project_ids { omitted.pages=SettledPages::default(); omitted.scoped_projects=project_ids.clone(); let older=omitted.older(); omitted.pages.sync_generation(owner.shell_generation,older.as_ref()); omitted.mounts.unmount(PAGES_KEY); }
        let Some((generation, before)) = omitted.pages.next_request() else { return };
        let resolve = self.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
        let runtime = self.runtime().clone();
        cx.spawn(async move |this, cx| {
            let Ok(target) = resolve.await else { return };
            let cursor = before.clone(); let requested_projects=project_ids.clone();
            if !this.read_with(cx,|state,_| target.is_current(&state.fleet) && state.scoped_ready(&owner,t3_core::auth::ORCHESTRATION_READ_SCOPE) && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE))).unwrap_or(false) { return; }
            let page = runtime.spawn(async move { target.client.request(GET_OLDER_SETTLED_THREADS, page_request(&cursor, requested_projects.as_deref())).await }).await
                .map_err(|error| error.to_string()).and_then(|reply| reply.map_err(|error| error.to_string())).and_then(decode_read);
            let _ = this.update(cx, |state, cx| {
                if !state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
                let omitted = state.sidebar.owner_omitted.entry(owner.clone()).or_default();
                if omitted.scoped_projects != project_ids || !omitted.pages.apply_page(generation, &before, page) { return; }
                omitted.mounts.mount(PAGES_KEY, generation, omitted.pages.reads.clone());
                state.rebuild_sidebar(cx); state.request_scoped_settled_page(owner, cx); cx.emit(AppEvent::Shell);
            });
        }).detach();
        cx.emit(AppEvent::Notice);
    }

    pub(crate) fn mount_scoped_search_rows(&mut self, owner: &t3_core::sidebar::grouping::EnvironmentOwner, read: Option<FamilyRead>, cx: &mut Context<Self>) {
        if self.scoped_environment(&owner.saved_id).is_none_or(|entry| entry.summary.owner != *owner) { return; }
        let omitted = self.sidebar.owner_omitted.entry(owner.clone()).or_default();
        match read.filter(|read| !read.threads.is_empty()) { Some(read) => omitted.mounts.mount(SEARCH_KEY, owner.shell_generation, vec![read]), None => omitted.mounts.unmount(SEARCH_KEY) }
        self.rebuild_sidebar(cx); cx.emit(AppEvent::Shell);
    }
}
