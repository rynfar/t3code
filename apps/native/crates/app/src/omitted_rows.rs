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
    page_task: Option<Task<()>>,
    /// The shell generation each family was last asked for, so a generation reads it once.
    family_requested: HashMap<String, u64>,
    scoped_projects: Option<Vec<String>>,
}

impl OmittedRows {
    fn suspend_pages(&mut self, generation: u64) {
        self.page_task = None;
        self.mounts.unmount(PAGES_KEY);
        let requested = self.pages.requested;
        self.pages = SettledPages::default();
        self.pages.requested = requested;
        let older = self.older();
        self.pages.sync_generation(generation, older.as_ref());
    }

    /// The cut and newest counts, while the shell omits older settled threads.
    pub fn older(&self) -> Option<OlderSettled> {
        self.counts.current()
    }
}

impl AppState {
    pub(crate) fn suspend_settled_pages(&mut self) {
        self.omitted.suspend_pages(self.shell.generation);
        for (owner, omitted) in &mut self.sidebar.owner_omitted {
            omitted.suspend_pages(owner.shell_generation);
        }
    }

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
        if !self.sidebar.shelves.settled_expanded { return; }
        let Some((generation, before)) = self.omitted.pages.next_request() else { return };
        let rpc = self.rpc(GET_OLDER_SETTLED_THREADS, page_request(&before, None));
        self.omitted.page_task = Some(cx.spawn(async move |this, cx| {
            let page = rpc.await.map_err(|error| error.to_string()).and_then(decode_read);
            let _ = this.update(cx, |state, cx| state.apply_settled_page(generation, &before, page, cx));
        }));
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
        let scopes = self.sidebar.owner_omitted.keys().filter(|owner| self.scoped_environment(&owner.saved_id).is_some_and(|entry| entry.summary.owner == **owner))
            .map(|owner| (owner.clone(),self.scoped_older_projects(owner,cx))).filter(|(_, scope)| !scope.as_ref().is_some_and(Vec::is_empty)).collect::<Vec<_>>();
        for (owner,scope) in &scopes {
            let omitted = self.sidebar.owner_omitted.get_mut(owner).unwrap();
            if omitted.scoped_projects != *scope { omitted.pages = SettledPages::default(); omitted.scoped_projects = scope.clone(); let older=omitted.older(); omitted.pages.sync_generation(owner.shell_generation,older.as_ref()); omitted.mounts.unmount(PAGES_KEY); }
        }
        let frontier = t3_core::sidebar::older_settled::highest_frontier(scopes.iter().filter_map(|(owner, _)| {
            let omitted = self.sidebar.owner_omitted.get(owner)?;
            omitted.pages.listing_frontier(omitted.older().as_ref())
        })).cloned();
        let Some(frontier) = frontier else { return; };
        let mut owners = Vec::new();
        for (owner, _) in scopes {
            let omitted = self.sidebar.owner_omitted.get_mut(&owner).unwrap();
            let older = omitted.older();
            if omitted.pages.listing_frontier(older.as_ref()) == Some(&frontier)
                && omitted.pages.grow(older.as_ref(),listed,visible) { owners.push(owner); }
        }
        for owner in owners { self.request_scoped_settled_page(owner, cx); }
    }

    fn request_scoped_settled_page(&mut self, owner: t3_core::sidebar::grouping::EnvironmentOwner, cx: &mut Context<Self>) {
        if !self.sidebar.shelves.settled_expanded { return; }
        if !self.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
        let project_ids = self.scoped_older_projects(&owner,cx);
        if project_ids.as_ref().is_some_and(Vec::is_empty) { return; }
        let omitted = self.sidebar.owner_omitted.entry(owner.clone()).or_default();
        if omitted.scoped_projects != project_ids { omitted.pages=SettledPages::default(); omitted.scoped_projects=project_ids.clone(); let older=omitted.older(); omitted.pages.sync_generation(owner.shell_generation,older.as_ref()); omitted.mounts.unmount(PAGES_KEY); }
        let Some((generation, before)) = omitted.pages.next_request() else { return };
        let resolve = self.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
        let runtime = self.runtime().clone();
        let page_owner = owner.clone();
        let task = cx.spawn(async move |this, cx| {
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
        });
        self.sidebar.owner_omitted.get_mut(&page_owner).unwrap().page_task = Some(task);
        cx.emit(AppEvent::Notice);
    }

    pub(crate) fn mount_scoped_search_rows(&mut self, owner: &t3_core::sidebar::grouping::EnvironmentOwner, read: Option<FamilyRead>, cx: &mut Context<Self>) {
        if self.scoped_environment(&owner.saved_id).is_none_or(|entry| entry.summary.owner != *owner) { return; }
        let omitted = self.sidebar.owner_omitted.entry(owner.clone()).or_default();
        match read.filter(|read| !read.threads.is_empty()) { Some(read) => omitted.mounts.mount(SEARCH_KEY, owner.shell_generation, vec![read]), None => omitted.mounts.unmount(SEARCH_KEY) }
        self.rebuild_sidebar(cx); cx.emit(AppEvent::Shell);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};

    #[gpui::test]
    async fn scoped_shelf_reads_only_the_highest_frontier_then_hands_off(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut active) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx,runtime.handle().clone()));
        let (shell, config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let sample = shell.snapshot.as_ref().unwrap().threads[0].clone();
        let settled = |id: &str, at| {
            let mut thread = sample.clone();
            thread.id = id.into(); thread.lineage.root_thread_id = id.into();
            thread.settled_override = Some("settled".into());
            thread.settled_at = Some(t3_core::sidebar::time::format_iso(at));
            thread
        };
        let mut fixtures = Vec::new();
        let mut event_senders = Vec::new();
        for (id, cut) in [("a",100_000),("b",80_000)] {
            let (client, fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap());
            let mut shell = shell.clone();
            let snapshot = shell.snapshot.as_mut().unwrap();
            snapshot.snapshot_sequence = 1;
            snapshot.threads = if id == "b" { (0..10).map(|index| settled(&format!("old-{index}"),50_000-index)).collect() } else { Vec::new() };
            snapshot.older_settled = Some(serde_json::from_value(serde_json::json!({"before":t3_core::sidebar::time::format_iso(cut),"asOfSequence":1,"counts":[{"projectId":"checkout-b","count":50}]})).unwrap());
            let mut config = config.clone();
            config.config.as_mut().unwrap().environment.environment_id = format!("server-{id}");
            let (sent, loaded) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Shell && state.scoped_environment(id).is_some() {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            let install = state.update(cx, |state, _| state.connections.install_fixture(id,client,shell,config,ConnectionStatus::Connected));
            event_senders.push(runtime.spawn(install).await.unwrap()); loaded.await.unwrap();
            fixtures.push(fixture);
        }
        let mut a = fixtures.remove(0); let mut b = fixtures.remove(0);
        state.update(cx, |state, cx| {
            state.sidebar.shelves.settled_visible = 35;
            state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx);
        });
        let (method, payload, reply) = a.next_request().await;
        assert_eq!(method,GET_OLDER_SETTLED_THREADS);
        assert_eq!(payload["limit"],25);
        assert_eq!(payload["before"]["settledAt"],t3_core::sidebar::time::format_iso(100_000));
        let owner_b = state.read_with(cx, |state, _| state.scoped_environment("b").unwrap().summary.owner.clone());
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&owner_b,t3_core::auth::ORCHESTRATION_READ_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap(); cx.run_until_parked();
        use futures::FutureExt;
        assert!(b.next_request().now_or_never().is_none(),"lower frontier must not read while the highest page is pending");
        let page = (0..25).map(|index| settled(&format!("a-{index:02}"),90_000-index*1000)).collect::<Vec<_>>();
        let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.sidebar.owner_omitted.iter().any(|(owner,omitted)| owner.saved_id == "a" && omitted.pages.reads.len() == 1) {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Ok(serde_json::json!({"snapshotSequence":1,"threads":page}))).unwrap(); applied.await.unwrap();
        let (method,payload,reply) = b.next_request().await;
        assert_eq!(method,GET_OLDER_SETTLED_THREADS);
        assert_eq!(payload["limit"],25);
        assert_eq!(payload["before"]["settledAt"],t3_core::sidebar::time::format_iso(80_000));
        state.read_with(cx, |state, _| {
            assert_eq!(state.sidebar.fleet.listed_settled_count,11);
            assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
        });
        reply.send(Err(t3_client::session::RpcError::Closed)).unwrap();
        assert!(!active.take_commands().contains(&GET_OLDER_SETTLED_THREADS));
        state.update(cx, |state, _| state.stop_pump());
        drop(event_senders);
    }

    #[gpui::test]
    async fn collapsed_scoped_shelf_releases_its_loaded_page_mount(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx,runtime.handle().clone()));
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap());
        let (mut shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let sample = shell.snapshot.as_ref().unwrap().threads[0].clone();
        let snapshot = shell.snapshot.as_mut().unwrap();
        snapshot.snapshot_sequence = 1;
        snapshot.threads.clear();
        snapshot.older_settled = Some(serde_json::from_value(serde_json::json!({
            "before":t3_core::sidebar::time::format_iso(100_000),"asOfSequence":1,"counts":[{"projectId":"checkout-b","count":50}]
        })).unwrap());
        config.config.as_mut().unwrap().environment.environment_id = "collapse-server".into();
        let (sent, loaded) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("collapse-fixture").is_some() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let mut next_snapshot = shell.snapshot.as_ref().unwrap().clone();
        next_snapshot.snapshot_sequence = 2;
        let install = state.update(cx, |state, _| state.connections.install_fixture("collapse-fixture",client,shell,config,ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method,GET_OLDER_SETTLED_THREADS);
        assert_eq!(payload["limit"],25);
        let page = (0..25).map(|index| {
            let mut thread = sample.clone();
            thread.id = format!("page-{index:02}"); thread.lineage.root_thread_id = thread.id.clone();
            thread.settled_override = Some("settled".into());
            thread.settled_at = Some(t3_core::sidebar::time::format_iso(90_000-index*1000));
            thread
        }).collect::<Vec<_>>();
        let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.sidebar.owner_omitted.iter().any(|(owner,omitted)| owner.saved_id == "collapse-fixture" && omitted.pages.reads.len() == 1) {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Ok(serde_json::json!({"snapshotSequence":1,"threads":page}))).unwrap(); applied.await.unwrap();
        let owner = state.read_with(cx, |state, _| state.scoped_environment("collapse-fixture").unwrap().summary.owner.clone());
        state.read_with(cx, |state, _| {
            let omitted = state.sidebar.owner_omitted.get(&owner).unwrap();
            assert_eq!(omitted.pages.reads.len(),1);
            assert_eq!(omitted.mounts.generation_of(PAGES_KEY),Some(owner.shell_generation));
        });
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        state.read_with(cx, |state, _| {
            assert!(!state.sidebar.shelves.settled_expanded);
            assert_eq!(state.sidebar.owner_omitted.get(&owner).unwrap().mounts.generation_of(PAGES_KEY),None,"collapsing the scoped Settled shelf releases its page consumer");
        });
        let family = family_key("independent-family");
        state.update(cx, |state, _| {
            state.sidebar.owner_omitted.get_mut(&owner).unwrap().mounts.mount(&family,owner.shell_generation,vec![FamilyRead { snapshot_sequence:1,threads:vec![sample.clone()] }]);
        });
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        let (_,old_payload,old_reply) = fixture.next_request().await;
        assert_eq!(old_payload["limit"],25);
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        state.read_with(cx, |state, _| assert!(state.sidebar.owner_omitted.get(&owner).unwrap().mounts.keys().any(|key| key == family)));
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        let (_,new_payload,new_reply) = fixture.next_request().await;
        assert_eq!(new_payload,old_payload,"reopening restarts at the same cut without accepting the old consumer's reply");
        let mut stale = sample.clone(); stale.id = "stale-page".into(); stale.lineage.root_thread_id = stale.id.clone();
        stale.settled_override = Some("settled".into()); stale.settled_at = Some(t3_core::sidebar::time::format_iso(90_000));
        let mut fresh = stale.clone(); fresh.id = "fresh-page".into(); fresh.lineage.root_thread_id = fresh.id.clone();
        let _ = old_reply.send(Ok(serde_json::json!({"snapshotSequence":1,"threads":[stale]})));
        let (sent, fresh_applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _fresh_applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.sidebar.owner_omitted.values().any(|omitted| omitted.pages.reads.iter().any(|read| read.threads.iter().any(|thread| thread.id == "fresh-page"))) {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        new_reply.send(Ok(serde_json::json!({"snapshotSequence":1,"threads":[fresh]}))).unwrap(); fresh_applied.await.unwrap();
        state.read_with(cx, |state, _| {
            let omitted = state.sidebar.owner_omitted.get(&owner).unwrap();
            assert!(omitted.pages.reads.iter().flat_map(|read| &read.threads).all(|thread| thread.id != "stale-page"));
            assert!(omitted.mounts.keys().any(|key| key == family));
        });
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        let (sent, snapshotted) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _snapshotted = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("collapse-fixture").is_some_and(|entry| entry.summary.snapshot_sequence == Some(2)) {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::Snapshot { snapshot:Box::new(next_snapshot),resolved_repository_identity_roots:None })).await.unwrap(); snapshotted.await.unwrap();
        let current = state.read_with(cx, |state, _| state.scoped_environment("collapse-fixture").unwrap().summary.owner.clone());
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&current,t3_core::auth::ORCHESTRATION_READ_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap(); cx.run_until_parked();
        use futures::FutureExt;
        assert!(fixture.next_request().now_or_never().is_none(),"a collapsed snapshot must not replay page reads");
        state.read_with(cx, |state, _| {
            let omitted = state.sidebar.owner_omitted.get(&current).unwrap();
            assert_eq!(omitted.mounts.generation_of(PAGES_KEY),None);
            assert!(omitted.mounts.keys().any(|key| key == family));
        });
        state.update(cx, |state, cx| state.toggle_shelf(t3_core::sidebar::Shelf::Settled,cx));
        let (_,payload,reply) = fixture.next_request().await;
        assert_eq!(payload["limit"],25);
        assert_eq!(payload["before"]["settledAt"],t3_core::sidebar::time::format_iso(100_000));
        reply.send(Err(t3_client::session::RpcError::Closed)).unwrap();
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
    }
}
