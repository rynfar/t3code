//! The preview browser a server hosts in headless Chromium and streams to clients without a local
//! browser runtime. Wire types port `packages/contracts/src/preview.ts`; the per-thread session
//! reducer ports `apps/web/src/previewStateStore.ts` (`applyPreviewServerEvent`,
//! `reconcilePreviewServerSessions`, `applyPreviewServerSnapshot`, `updatePreviewServerSnapshot`,
//! `beginPreviewSessionClose`, `cancelPreviewSessionClose`, `setActivePreviewTab`), with
//! `shouldShowPreviewEmptyState`, the tab title from `RightPanelTabs.tsx` (`surfaceTitle`) and
//! `freshlyRevealedServerTabs` from `components/preview/agentOpenedPreviews.ts`. The stream
//! protocol is in `stream`, address bar and history in `address`.

pub mod address;
pub mod stream;

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use crate::right_panel::RightPanels;

pub mod method {
    pub const OPEN: &str = "preview.open";
    pub const ADJUST: &str = "preview.adjust";
    pub const CLOSE: &str = "preview.close";
    pub const LIST: &str = "preview.list";
    pub const SUBSCRIBE_EVENTS: &str = "subscribePreviewEvents";
    pub const SUBSCRIBE_DISCOVERED_SERVERS: &str = "subscribeDiscoveredLocalServers";
}

/// `AuthPreviewOperateScope`: open, drive and close previews.
pub const PREVIEW_OPERATE_SCOPE: &str = "preview:operate";

/// `PREVIEW_ZOOM_LEVELS`, shared with desktop Chromium's zoom steps.
pub const ZOOM_LEVELS: [f64; 17] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];
pub const DEFAULT_ZOOM: f64 = 1.0;

/// `PREVIEW_RECENT_URL_LIMIT`.
const RECENT_URL_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FileDownload {
    pub id: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
}

/// `PreviewNavStatus`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "_tag")]
pub enum NavStatus {
    Idle,
    Loading {
        url: String,
        #[serde(default)]
        title: String,
    },
    Success {
        url: String,
        #[serde(default)]
        title: String,
    },
    LoadFailed {
        url: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        code: i64,
        #[serde(default)]
        description: String,
        #[serde(default)]
        download: Option<FileDownload>,
    },
    #[serde(other)]
    Unknown,
}

impl NavStatus {
    pub fn url(&self) -> Option<&str> {
        match self {
            NavStatus::Loading { url, .. } | NavStatus::Success { url, .. } | NavStatus::LoadFailed { url, .. } => Some(url),
            NavStatus::Idle | NavStatus::Unknown => None,
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            NavStatus::Loading { title, .. } | NavStatus::Success { title, .. } | NavStatus::LoadFailed { title, .. } => Some(title),
            NavStatus::Idle | NavStatus::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RevealRequest {
    pub id: String,
    #[serde(default)]
    pub force: bool,
}

/// `PreviewSessionSnapshot`. The viewport setting passes through untouched: this client always
/// fills the panel and only reads whether it does.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub thread_id: String,
    pub tab_id: String,
    pub nav_status: NavStatus,
    #[serde(default)]
    pub can_go_back: bool,
    #[serde(default)]
    pub can_go_forward: bool,
    #[serde(default)]
    pub viewport: Option<Value>,
    #[serde(default)]
    pub color_scheme: Option<String>,
    #[serde(default)]
    pub zoom_factor: Option<f64>,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub headless: Option<bool>,
    #[serde(default)]
    pub automation_owner: Option<String>,
    #[serde(default)]
    pub reveal: Option<bool>,
    #[serde(default)]
    pub reveal_request: Option<RevealRequest>,
    #[serde(default)]
    pub updated_at: String,
}

impl SessionSnapshot {
    pub fn decode(value: Value) -> Result<SessionSnapshot, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    pub fn is_server(&self) -> bool {
        self.runtime.as_deref() == Some("server")
    }

    /// A tab on the server's browser fills the panel unless the device toolbar chose a size.
    pub fn fills_panel(&self) -> bool {
        self.viewport.as_ref().is_none_or(|viewport| viewport.get("_tag").and_then(Value::as_str) == Some("fill"))
    }

    pub fn zoom(&self) -> f64 {
        self.zoom_factor.unwrap_or(DEFAULT_ZOOM)
    }
}

/// `shouldShowPreviewEmptyState`.
pub fn shows_empty_state(snapshot: Option<&SessionSnapshot>) -> bool {
    snapshot.is_none_or(|snapshot| snapshot.nav_status == NavStatus::Idle)
}

/// The tab label: the page title, else its host, else "Browser" (`surfaceTitle`).
pub fn tab_title(snapshot: Option<&SessionSnapshot>) -> String {
    let Some(status) = snapshot.map(|snapshot| &snapshot.nav_status) else { return "Browser".to_owned() };
    let (Some(url), Some(title)) = (status.url(), status.title()) else { return "Browser".to_owned() };
    if !title.trim().is_empty() {
        return title.to_owned();
    }
    host_label(url).unwrap_or_else(|| "Browser".to_owned())
}

/// `new URL(url).host`: the host with its port, `None` when unparseable or empty.
pub fn host_label(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str().filter(|host| !host.is_empty())?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// `describePreviewError` over `PREVIEW_ERROR_CODE_MESSAGES`: Chromium's net error in words.
pub fn describe_error(description: &str) -> &str {
    match description {
        "ERR_NAME_NOT_RESOLVED" | "ERR_NAME_RESOLUTION_FAILED" => "DNS address could not be found",
        "ERR_CONNECTION_REFUSED" => "Connection refused",
        "ERR_CONNECTION_RESET" => "Connection was reset",
        "ERR_CONNECTION_CLOSED" => "Connection was closed",
        "ERR_CONNECTION_TIMED_OUT" | "ERR_TIMED_OUT" => "Connection timed out",
        "ERR_INTERNET_DISCONNECTED" => "No internet connection",
        "ERR_CERT_AUTHORITY_INVALID" => "Certificate authority is not trusted",
        "ERR_CERT_COMMON_NAME_INVALID" => "Certificate hostname mismatch",
        "ERR_CERT_DATE_INVALID" => "Certificate is expired or not yet valid",
        "ERR_TOO_MANY_REDIRECTS" => "Too many redirects",
        "" => "Network error",
        other => other,
    }
}

/// The unreachable page's error label: the description, else `ERR_<code>`.
pub fn error_label(code: i64, description: &str) -> String {
    if description.is_empty() {
        match code.unsigned_abs() {
            0 => "ERR_FAILED".to_owned(),
            code => format!("ERR_{code}"),
        }
    } else {
        description.to_owned()
    }
}

/// The next zoom step from the current factor (`stepServerZoom`): `0` resets.
pub fn step_zoom(current: f64, direction: i8) -> f64 {
    if direction == 0 {
        return DEFAULT_ZOOM;
    }
    let index = ZOOM_LEVELS.iter().position(|level| (level - current).abs() < 1e-9).unwrap_or(7) as i64;
    let next = (index + i64::from(direction.signum())).clamp(0, ZOOM_LEVELS.len() as i64 - 1);
    ZOOM_LEVELS[next as usize]
}

/// `PreviewEvent`: every variant carries the epoch and revision that order it against lists.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewEvent {
    pub thread_id: String,
    pub tab_id: String,
    pub created_at: String,
    pub server_epoch: String,
    pub revision: u64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Opened(SessionSnapshot),
    Navigated(SessionSnapshot),
    Resized(SessionSnapshot),
    Failed { url: String, title: String, code: i64, description: String, download: Option<FileDownload> },
    Closed,
    /// A newer server's event: it still advances the revision.
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    #[serde(rename = "type")]
    kind: String,
    thread_id: String,
    tab_id: String,
    #[serde(default)]
    created_at: String,
    server_epoch: String,
    revision: u64,
    #[serde(default)]
    snapshot: Option<Value>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    code: i64,
    #[serde(default)]
    description: String,
    #[serde(default)]
    download: Option<FileDownload>,
}

impl PreviewEvent {
    pub fn decode(value: Value) -> Result<PreviewEvent, String> {
        let raw: RawEvent = serde_json::from_value(value).map_err(|error| error.to_string())?;
        let snapshot = || raw.snapshot.clone().ok_or_else(|| format!("{} event without a snapshot", raw.kind)).and_then(SessionSnapshot::decode);
        let kind = match raw.kind.as_str() {
            "opened" => EventKind::Opened(snapshot()?),
            "navigated" => EventKind::Navigated(snapshot()?),
            "resized" => EventKind::Resized(snapshot()?),
            "failed" => EventKind::Failed {
                url: raw.url.clone(),
                title: raw.title.clone(),
                code: raw.code,
                description: raw.description.clone(),
                download: raw.download.clone(),
            },
            "closed" => EventKind::Closed,
            _ => EventKind::Unknown,
        };
        Ok(PreviewEvent {
            thread_id: raw.thread_id,
            tab_id: raw.tab_id,
            created_at: raw.created_at,
            server_epoch: raw.server_epoch,
            revision: raw.revision,
            kind,
        })
    }
}

/// `PreviewListResult`; a session this build cannot read is skipped, not the whole list.
#[derive(Debug, Clone, PartialEq)]
pub struct ListResult {
    pub sessions: Vec<SessionSnapshot>,
    pub server_epoch: String,
    pub revision: u64,
}

impl ListResult {
    pub fn decode(value: Value) -> Result<ListResult, String> {
        let server_epoch = value.get("serverEpoch").and_then(Value::as_str).ok_or("preview list without serverEpoch")?.to_owned();
        let revision = value.get("revision").and_then(Value::as_u64).unwrap_or(0);
        let sessions = value
            .get("sessions")
            .and_then(Value::as_array)
            .map(|sessions| sessions.iter().filter_map(|session| SessionSnapshot::decode(session.clone()).ok()).collect())
            .unwrap_or_default();
        Ok(ListResult { sessions, server_epoch, revision })
    }
}

/// `ThreadPreviewState` without the desktop overlay, which only an Electron host has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadPreviews {
    /// In the order the server reported them, as the web's record keeps insertion order.
    pub sessions: Vec<SessionSnapshot>,
    /// Tabs this client is closing; stale lists and events must not bring them back.
    suppressed: HashSet<String>,
    pub active_tab_id: Option<String>,
    pub recently_seen_urls: Vec<String>,
    pub list_loaded: bool,
    pub server_epoch: Option<String>,
    pub server_revision: u64,
}

fn remember(urls: &mut Vec<String>, url: &str) {
    urls.retain(|entry| entry != url);
    urls.insert(0, url.to_owned());
    urls.truncate(RECENT_URL_LIMIT);
}

fn remember_snapshot(urls: &mut Vec<String>, snapshot: &SessionSnapshot) {
    if let Some(url) = snapshot.nav_status.url() {
        remember(urls, url);
    }
}

impl ThreadPreviews {
    pub fn session(&self, tab_id: &str) -> Option<&SessionSnapshot> {
        self.sessions.iter().find(|session| session.tab_id == tab_id)
    }

    /// The tab the panel shows when its surface names none.
    pub fn active(&self) -> Option<&SessionSnapshot> {
        self.active_tab_id.as_deref().and_then(|tab| self.session(tab))
    }

    fn upsert(&mut self, snapshot: SessionSnapshot) {
        match self.sessions.iter_mut().find(|session| session.tab_id == snapshot.tab_id) {
            Some(existing) => *existing = snapshot,
            None => self.sessions.push(snapshot),
        }
    }

    fn latest_tab(&self) -> Option<String> {
        self.sessions.iter().max_by(|a, b| a.updated_at.cmp(&b.updated_at)).map(|session| session.tab_id.clone())
    }

    fn remove(&mut self, tab_id: &str) {
        let Some(index) = self.sessions.iter().position(|session| session.tab_id == tab_id) else { return };
        self.sessions.remove(index);
        if self.active_tab_id.as_deref() == Some(tab_id) || self.active().is_none() {
            self.active_tab_id = self.latest_tab();
        }
    }

    /// An event from another epoch means the server restarted: the caller lists again.
    pub fn needs_relist(&self, event: &PreviewEvent) -> bool {
        self.server_epoch.as_ref().is_some_and(|epoch| *epoch != event.server_epoch)
    }

    /// `applyPreviewServerEvent`.
    pub fn apply_event(&mut self, event: &PreviewEvent) -> bool {
        if self.needs_relist(event) || event.revision < self.server_revision {
            return false;
        }
        let before = self.clone();
        match &event.kind {
            EventKind::Opened(snapshot) | EventKind::Navigated(snapshot) | EventKind::Resized(snapshot) => {
                if !self.suppressed.contains(&snapshot.tab_id) {
                    remember_snapshot(&mut self.recently_seen_urls, snapshot);
                    let opened = matches!(event.kind, EventKind::Opened(_));
                    if opened || self.active_tab_id.is_none() {
                        self.active_tab_id = Some(snapshot.tab_id.clone());
                    }
                    self.upsert(snapshot.clone());
                }
            }
            EventKind::Failed { url, title, code, description, download } => {
                if let Some(session) = self.sessions.iter_mut().find(|session| session.tab_id == event.tab_id) {
                    session.nav_status = NavStatus::LoadFailed {
                        url: url.clone(),
                        title: title.clone(),
                        code: *code,
                        description: description.clone(),
                        download: download.clone(),
                    };
                    session.updated_at = event.created_at.clone();
                }
            }
            EventKind::Closed => {
                self.remove(&event.tab_id);
                self.suppressed.remove(&event.tab_id);
            }
            EventKind::Unknown => {}
        }
        self.server_epoch = Some(event.server_epoch.clone());
        self.server_revision = event.revision;
        *self != before
    }

    /// `reconcilePreviewServerSessions`: the list is the truth for its revision. A session this
    /// client saw updated later than the list keeps the newer copy.
    pub fn reconcile(&mut self, result: &ListResult) -> bool {
        let same_server = self.server_epoch.as_deref() == Some(result.server_epoch.as_str());
        if same_server && result.revision < self.server_revision {
            return false;
        }
        let before = self.clone();
        let suppressed = if same_server { std::mem::take(&mut self.suppressed) } else { HashSet::new() };
        let mut sessions = Vec::with_capacity(result.sessions.len());
        for snapshot in &result.sessions {
            if suppressed.contains(&snapshot.tab_id) {
                continue;
            }
            let existing = if same_server { self.session(&snapshot.tab_id) } else { None };
            let next = match existing {
                Some(existing) if existing.updated_at > snapshot.updated_at => existing.clone(),
                _ => snapshot.clone(),
            };
            remember_snapshot(&mut self.recently_seen_urls, &next);
            sessions.push(next);
        }
        self.sessions = sessions;
        if self.active().is_none() {
            self.active_tab_id = self.latest_tab();
        }
        self.suppressed = suppressed.into_iter().filter(|tab| result.sessions.iter().any(|session| session.tab_id == *tab)).collect();
        self.list_loaded = true;
        self.server_epoch = Some(result.server_epoch.clone());
        self.server_revision = result.revision;
        *self != before
    }

    /// `applyPreviewServerSnapshot`: what `preview.open` returned, shown in front.
    pub fn apply_snapshot(&mut self, snapshot: SessionSnapshot) -> bool {
        if self.suppressed.contains(&snapshot.tab_id) || self.session(&snapshot.tab_id).is_some_and(|existing| existing.updated_at > snapshot.updated_at) {
            return false;
        }
        let before = self.clone();
        remember_snapshot(&mut self.recently_seen_urls, &snapshot);
        self.active_tab_id = Some(snapshot.tab_id.clone());
        self.upsert(snapshot);
        *self != before
    }

    /// `updatePreviewServerSnapshot`: a background tab changes without coming to the front.
    pub fn update_snapshot(&mut self, snapshot: SessionSnapshot) -> bool {
        if self.suppressed.contains(&snapshot.tab_id) || self.session(&snapshot.tab_id).is_some_and(|existing| existing.updated_at > snapshot.updated_at) {
            return false;
        }
        let before = self.clone();
        remember_snapshot(&mut self.recently_seen_urls, &snapshot);
        let tab_id = snapshot.tab_id.clone();
        self.upsert(snapshot);
        if self.active().is_none() {
            self.active_tab_id = Some(tab_id);
        }
        *self != before
    }

    /// `beginPreviewSessionClose`: hides the tab before `preview.close` answers.
    pub fn begin_close(&mut self, tab_id: &str) -> bool {
        let before = self.clone();
        self.remove(tab_id);
        self.suppressed.insert(tab_id.to_owned());
        *self != before
    }

    /// `cancelPreviewSessionClose`: the close failed, so the tab comes back.
    pub fn cancel_close(&mut self, snapshot: Option<SessionSnapshot>, tab_id: &str) -> bool {
        if !self.suppressed.remove(tab_id) {
            return false;
        }
        if let Some(snapshot) = snapshot {
            remember_snapshot(&mut self.recently_seen_urls, &snapshot);
            self.active_tab_id = Some(snapshot.tab_id.clone());
            self.upsert(snapshot);
        }
        true
    }

    /// `setActivePreviewTab`.
    pub fn set_active(&mut self, tab_id: &str) -> bool {
        if self.session(tab_id).is_none() || self.active_tab_id.as_deref() == Some(tab_id) {
            return false;
        }
        self.active_tab_id = Some(tab_id.to_owned());
        true
    }

    /// `rememberPreviewUrl`.
    pub fn remember_url(&mut self, url: &str) {
        if !url.trim().is_empty() {
            remember(&mut self.recently_seen_urls, url);
        }
    }

    /// `followPreviewSessions`: the tabs the panel should list, and the ones a server tab asked to
    /// keep out of it (`reveal: false`). `None` until the first list, when a missing session is
    /// not yet a closed one.
    pub fn panel_tabs(&self) -> Option<(Vec<String>, HashSet<String>)> {
        if !self.list_loaded {
            return None;
        }
        let tabs = self.sessions.iter().map(|session| session.tab_id.clone()).collect();
        let hidden = self.sessions.iter().filter(|session| session.is_server() && session.reveal == Some(false)).map(|session| session.tab_id.clone()).collect();
        Some((tabs, hidden))
    }

    /// `freshlyRevealedServerTabs`: server tabs that are new, or carry a new reveal request, since
    /// `previous`. The first list is the baseline, so a reload never reopens dismissed previews.
    pub fn freshly_revealed<'a>(&'a self, previous: &ThreadPreviews) -> Vec<&'a SessionSnapshot> {
        if !previous.list_loaded {
            return Vec::new();
        }
        self.sessions
            .iter()
            .filter(|session| session.is_server() && session.reveal == Some(true) && !previous.suppressed.contains(&session.tab_id))
            .filter(|session| {
                let before = previous.session(&session.tab_id);
                match &session.reveal_request {
                    Some(request) => before.and_then(|before| before.reveal_request.as_ref()).is_none_or(|old| old.id != request.id),
                    None => before.is_none(),
                }
            })
            .collect()
    }
}

/// What the shown thread's browser tabs need: follow its sessions (once listed), and bring
/// `fronted` tabs (forced reveals, popups) to the front.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabsEdit {
    pub tabs: Option<(Vec<String>, HashSet<String>)>,
    pub fronted: Vec<String>,
}

impl TabsEdit {
    pub fn apply(&self, panels: &mut RightPanels, thread_id: &str) -> bool {
        let mut changed = false;
        if let Some((tabs, hidden)) = &self.tabs {
            changed |= panels.reconcile_browser_surfaces(thread_id, tabs, hidden);
        }
        for tab in &self.fronted {
            changed |= panels.open_browser(thread_id, Some(tab));
        }
        changed
    }
}

/// What a batch of events did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// Threads whose sessions changed.
    pub changed: Vec<String>,
    /// An event came from another server epoch, so the shown thread lists again.
    pub relist: bool,
    pub edit: Option<TabsEdit>,
}

/// The sessions of every thread this client follows, and which thread the panel shows: one
/// `usePreviewSession` per thread with `followPreviewSessions` for the shown one. A thread's
/// sessions keep changing while it is hidden, so showing it again catches its tabs up.
#[derive(Debug, Clone, Default)]
pub struct PreviewThreads {
    threads: std::collections::HashMap<String, ThreadPreviews>,
    shown: Option<String>,
    /// A page's popup, fronted once its session is known (`whenPreviewTabKnown`).
    pending_popup: Option<(String, String)>,
}

impl PreviewThreads {
    pub fn thread(&self, thread_id: &str) -> Option<&ThreadPreviews> {
        self.threads.get(thread_id)
    }

    /// Follows `thread_id` from now on.
    pub fn thread_mut(&mut self, thread_id: &str) -> &mut ThreadPreviews {
        self.threads.entry(thread_id.to_owned()).or_default()
    }

    /// A new connection: nothing is known about any thread.
    pub fn clear(&mut self) {
        self.threads.clear();
        self.pending_popup = None;
    }

    /// A thread comes into view, or none does.
    pub fn show(&mut self, thread_id: Option<&str>) -> Option<TabsEdit> {
        self.shown = thread_id.map(str::to_owned);
        self.edit(thread_id?, None)
    }

    /// Events for followed threads; others are ignored.
    pub fn apply(&mut self, events: impl IntoIterator<Item = PreviewEvent>) -> Applied {
        let mut applied = Applied::default();
        let mut previous: Vec<(String, ThreadPreviews)> = Vec::new();
        for event in events {
            let Some(thread) = self.threads.get_mut(&event.thread_id) else { continue };
            if thread.needs_relist(&event) {
                applied.relist = true;
                continue;
            }
            let before = thread.clone();
            if thread.apply_event(&event) && !previous.iter().any(|(id, _)| *id == event.thread_id) {
                previous.push((event.thread_id.clone(), before));
            }
        }
        for (thread_id, before) in previous {
            if self.shown.as_deref() == Some(thread_id.as_str()) {
                applied.edit = self.edit(&thread_id, Some(&before));
            }
            applied.changed.push(thread_id);
        }
        applied
    }

    /// A list answered for `thread_id`: whether its sessions changed, whether to list again
    /// (an event overtook it), and the shown thread's edit, which follows the list even when
    /// the sessions did not change, because its tabs may lag them.
    pub fn listed(&mut self, thread_id: &str, list: &ListResult) -> (bool, bool, Option<TabsEdit>) {
        let thread = self.thread_mut(thread_id);
        let before = thread.clone();
        let changed = thread.reconcile(list);
        if !thread.list_loaded {
            return (changed, true, None);
        }
        (changed, false, self.edit(thread_id, changed.then_some(&before)))
    }

    /// The shown thread's edit after a change made here (open, close, cancel) from `before`.
    pub fn changed(&mut self, thread_id: &str, before: &ThreadPreviews) -> Option<TabsEdit> {
        self.edit(thread_id, Some(before))
    }

    /// The page opened a popup: fronted now if its session is known, else once it is.
    pub fn popup(&mut self, thread_id: &str, tab_id: &str) -> Option<TabsEdit> {
        if self.threads.get(thread_id).and_then(|thread| thread.session(tab_id)).is_some() {
            return (self.shown.as_deref() == Some(thread_id)).then(|| TabsEdit { tabs: None, fronted: vec![tab_id.to_owned()] });
        }
        self.pending_popup = Some((thread_id.to_owned(), tab_id.to_owned()));
        None
    }

    fn edit(&mut self, thread_id: &str, before: Option<&ThreadPreviews>) -> Option<TabsEdit> {
        if self.shown.as_deref() != Some(thread_id) {
            return None;
        }
        let next = self.threads.get(thread_id)?;
        let mut fronted: Vec<String> = before
            .map(|before| {
                next.freshly_revealed(before)
                    .into_iter()
                    .filter(|session| session.reveal_request.as_ref().is_some_and(|request| request.force))
                    .map(|session| session.tab_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Some((_, tab)) = self.pending_popup.take_if(|(thread, tab)| thread == thread_id && next.session(tab).is_some()) {
            fronted.push(tab);
        }
        let tabs = next.panel_tabs();
        (tabs.is_some() || !fronted.is_empty()).then_some(TabsEdit { tabs, fronted })
    }
}

/// `DiscoveredLocalServer`, as the empty state lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredServer {
    pub host: String,
    pub port: u16,
    pub url: String,
    #[serde(default)]
    pub process_name: Option<String>,
    #[serde(default)]
    pub pid: Option<i64>,
}

impl DiscoveredServer {
    /// `describeServer`.
    pub fn title(&self) -> &str {
        self.process_name.as_deref().unwrap_or("Listening")
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A `DiscoveredLocalServerList` update, sorted by port as `mergeServers` sorts scanner results.
pub fn decode_discovered_servers(value: &Value) -> Vec<DiscoveredServer> {
    let mut servers: Vec<DiscoveredServer> = value
        .get("servers")
        .and_then(Value::as_array)
        .map(|servers| servers.iter().filter_map(|server| serde_json::from_value(server.clone()).ok()).collect())
        .unwrap_or_default();
    servers.sort_by_key(|server| server.port);
    servers
}

#[cfg(test)]
mod tests;
