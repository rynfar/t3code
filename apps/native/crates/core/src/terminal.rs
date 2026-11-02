//! Thread terminals: wire types and payloads from `packages/contracts/src/terminal.ts`, the
//! attach and metadata reducers from `client-runtime/src/state/terminalSession.ts`, the session
//! lifecycle from `client-runtime/src/state/terminal.ts`, labels from
//! `packages/shared/src/terminalLabels.ts`, and launch locations from
//! `packages/shared/src/projectScripts.ts`. The output buffer is not ported: the native
//! emulator keeps scrollback, so the reducer reports resets and appends instead.

pub mod emulator;
mod input;
mod layout;

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

pub use input::{KeyInput, encode_key, encode_paste, paste_chunks, shortcut_data};
pub use layout::{
    MAX_TERMINALS_PER_GROUP, SplitDirection, TerminalGroup, TerminalLayouts, ThreadTerminalLayout,
    pick_existing_terminal, should_reconcile,
};

pub mod method {
    pub const OPEN: &str = "terminal.open";
    pub const ATTACH: &str = "terminal.attach";
    pub const OBSERVE: &str = "terminal.observe";
    pub const WRITE: &str = "terminal.write";
    pub const RESIZE: &str = "terminal.resize";
    pub const CLEAR: &str = "terminal.clear";
    pub const CLOSE: &str = "terminal.close";
    pub const SUBSCRIBE_METADATA: &str = "subscribeTerminalMetadata";
}

pub const DEFAULT_TERMINAL_ID: &str = "term-1";
/// `DEFAULT_MAX_TERMINAL_BUFFER_BYTES`: history replayed into a fresh emulator is cut to this.
pub const MAX_BUFFER_BYTES: usize = 512 * 1024;
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;

/// `ThreadTerminalDrawer.tsx` and `types.ts` drawer metrics.
pub const DEFAULT_DRAWER_HEIGHT: f32 = 280.;
pub const MIN_DRAWER_HEIGHT: f32 = 180.;
const MAX_DRAWER_HEIGHT_RATIO: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Starting,
    Running,
    Exited,
    Error,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Grid { cols: cols.clamp(1, MAX_COLS), rows: rows.clamp(1, MAX_ROWS) }
    }

    /// Each axis is the smaller of the two: the PTY grid lags a local resize, and a surface
    /// never draws past its own fit (`surface.ts` `applyGrid`).
    pub fn fit_within(shared: Option<Grid>, measured: Grid) -> Grid {
        match shared {
            Some(shared) => Grid { cols: shared.cols.min(measured.cols), rows: shared.rows.min(measured.rows) },
            None => measured,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub thread_id: String,
    pub terminal_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub worktree_path: Option<String>,
    pub status: SessionStatus,
    #[serde(default)]
    pub history: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub thread_id: String,
    pub terminal_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub worktree_path: Option<String>,
    pub status: SessionStatus,
    #[serde(default)]
    pub has_running_subprocess: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub updated_at: String,
}

/// `TerminalAttachStreamEvent`, the items of `terminal.attach` and `terminal.observe`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AttachEvent {
    Snapshot {
        snapshot: SessionSnapshot,
    },
    Output {
        data: String,
    },
    Exited {
        #[serde(default)]
        exit_code: Option<i64>,
    },
    Closed,
    Error {
        message: String,
    },
    Cleared,
    Restarted {
        snapshot: SessionSnapshot,
    },
    Activity {
        #[serde(default)]
        has_running_subprocess: bool,
        #[serde(default)]
        label: String,
        #[serde(default)]
        cols: Option<u16>,
        #[serde(default)]
        rows: Option<u16>,
    },
    #[serde(other)]
    Unknown,
}

/// `TerminalMetadataStreamEvent`, the items of `subscribeTerminalMetadata`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MetadataEvent {
    Snapshot { terminals: Vec<TerminalSummary> },
    Upsert { terminal: TerminalSummary },
    Remove { thread_id: String, terminal_id: String },
    #[serde(other)]
    Unknown,
}

/// `applyTerminalMetadataStreamEvent`. Upserts move the terminal to the end, as the web does.
pub fn apply_metadata(current: &mut Vec<TerminalSummary>, event: MetadataEvent) {
    match event {
        MetadataEvent::Snapshot { terminals } => *current = terminals,
        MetadataEvent::Remove { thread_id, terminal_id } => {
            current.retain(|t| t.thread_id != thread_id || t.terminal_id != terminal_id);
        }
        MetadataEvent::Upsert { terminal } => {
            current.retain(|t| t.thread_id != terminal.thread_id || t.terminal_id != terminal.terminal_id);
            current.push(terminal);
        }
        MetadataEvent::Unknown => {}
    }
}

/// One thread's terminals in the order `terminalSessions.ts` lists them: terminal id with
/// numeric collation, so `term-10` follows `term-9`.
pub fn thread_terminals<'a>(summaries: &'a [TerminalSummary], thread_id: &str) -> Vec<&'a TerminalSummary> {
    let mut terminals: Vec<_> = summaries.iter().filter(|t| t.thread_id == thread_id).collect();
    terminals.sort_by(|a, b| natural_cmp(&a.terminal_id, &b.terminal_id));
    terminals
}

pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left.chars().peekable(), right.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// The client-side session status. `Closed` is also the state before the first snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferStatus {
    Starting,
    Running,
    Exited,
    Error,
    Closed,
}

impl BufferStatus {
    pub fn is_live(self) -> bool {
        matches!(self, BufferStatus::Running | BufferStatus::Starting)
    }

    fn from_session(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Running => BufferStatus::Running,
            SessionStatus::Exited => BufferStatus::Exited,
            SessionStatus::Error => BufferStatus::Error,
            SessionStatus::Starting | SessionStatus::Unknown => BufferStatus::Starting,
        }
    }
}

/// What an attach event does to the emulator's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputOp {
    None,
    /// Start over from this history (snapshot, restart, clear).
    Reset(String),
    Append(String),
}

/// `TerminalBufferState` without the retained text.
#[derive(Debug, Clone, PartialEq)]
pub struct BufferState {
    pub status: BufferStatus,
    /// The PTY grid shared by every viewer; `None` when the server predates it.
    pub grid: Option<Grid>,
    pub error: Option<String>,
    pub has_running_subprocess: bool,
    pub version: u64,
    pub lifecycle_version: u64,
}

impl Default for BufferState {
    fn default() -> Self {
        BufferState {
            status: BufferStatus::Closed,
            grid: None,
            error: None,
            has_running_subprocess: false,
            version: 0,
            lifecycle_version: 0,
        }
    }
}

impl BufferState {
    /// `applyTerminalAttachStreamEvent`.
    pub fn apply(&mut self, event: AttachEvent) -> OutputOp {
        match event {
            AttachEvent::Snapshot { snapshot } => {
                if self.version > 0 {
                    self.lifecycle_version += 1;
                }
                self.reset_from(snapshot)
            }
            AttachEvent::Restarted { snapshot } => {
                self.lifecycle_version += 1;
                self.reset_from(snapshot)
            }
            AttachEvent::Output { data } => {
                if self.status == BufferStatus::Closed {
                    self.status = BufferStatus::Running;
                }
                self.error = None;
                self.version += 1;
                if data.is_empty() { OutputOp::None } else { OutputOp::Append(data) }
            }
            AttachEvent::Cleared => {
                self.error = None;
                self.version += 1;
                OutputOp::Reset(String::new())
            }
            AttachEvent::Exited { .. } => self.set_status(BufferStatus::Exited, None),
            AttachEvent::Closed => self.set_status(BufferStatus::Closed, None),
            AttachEvent::Error { message } => self.set_status(BufferStatus::Error, Some(message)),
            AttachEvent::Activity { has_running_subprocess, cols, rows, .. } => {
                self.has_running_subprocess = has_running_subprocess;
                if let (Some(cols), Some(rows)) = (cols, rows) {
                    self.grid = Some(Grid::new(cols, rows));
                }
                OutputOp::None
            }
            AttachEvent::Unknown => OutputOp::None,
        }
    }

    fn reset_from(&mut self, snapshot: SessionSnapshot) -> OutputOp {
        if let (Some(cols), Some(rows)) = (snapshot.cols, snapshot.rows) {
            self.grid = Some(Grid::new(cols, rows));
        }
        self.status = BufferStatus::from_session(snapshot.status);
        self.error = None;
        self.version += 1;
        OutputOp::Reset(trim_to_bytes(snapshot.history, MAX_BUFFER_BYTES))
    }

    fn set_status(&mut self, status: BufferStatus, error: Option<String>) -> OutputOp {
        self.status = status;
        self.error = error;
        self.version += 1;
        OutputOp::None
    }
}

/// Keeps the last `max` bytes, starting on a character boundary.
pub fn trim_to_bytes(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
    text
}

/// `TerminalLifecycleState`: whether an ended shell is closed, reopened, or left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub was_running: bool,
    /// The user has seen this terminal without operate permission.
    pub observed: bool,
    pub reopened: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    Observed,
    Detached { can_operate: bool },
    Status { status: BufferStatus, version: u64 },
    ReopenFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleCommand {
    /// The shell ended while attached: close it and drop it from the drawer.
    Exit,
    /// An explicit visit found an ended shell: start it again.
    Reopen,
}

impl Lifecycle {
    /// `transitionTerminalLifecycle`.
    pub fn transition(&mut self, event: LifecycleEvent) -> Option<LifecycleCommand> {
        match event {
            LifecycleEvent::Observed => {
                self.observed = true;
                None
            }
            LifecycleEvent::Detached { can_operate } => {
                self.observed = self.observed || (!can_operate && self.was_running);
                self.was_running = false;
                None
            }
            LifecycleEvent::ReopenFailed => {
                self.reopened = false;
                None
            }
            LifecycleEvent::Status { status, version } => {
                if version == 0 {
                    return None;
                }
                if status.is_live() {
                    self.was_running = true;
                    self.reopened = false;
                    return None;
                }
                if !matches!(status, BufferStatus::Closed | BufferStatus::Exited) {
                    return None;
                }
                if self.was_running {
                    self.was_running = false;
                    self.reopened = true;
                    return Some(LifecycleCommand::Exit);
                }
                if self.observed || self.reopened {
                    return None;
                }
                self.reopened = true;
                Some(LifecycleCommand::Reopen)
            }
        }
    }
}

/// A terminal surface's font (`resolveTerminalFontPreference`, `resolveTerminalFontSizePreference`
/// and `terminalFontOptions`): the code font and size, or the terminal's own once typography is
/// split (`t3code:typography-advanced`).
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTypography {
    /// The first family of the preference; `None` keeps the default code font.
    pub family: Option<String>,
    pub size: f32,
}

/// A `ClientSettingsSchema` font size: an integer in `min..=max`, else `default`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSizeRange {
    pub default: f32,
    pub min: f32,
    pub max: f32,
}

impl TerminalTypography {
    pub fn resolve(settings: &serde_json::Map<String, Value>, advanced: bool, code: FontSizeRange, terminal: FontSizeRange) -> Self {
        let (family_key, size_key, range) =
            if advanced { ("fontFamilyTerminal", "fontSizeTerminal", terminal) } else { ("fontFamilyCode", "fontSizeCode", code) };
        let size = settings
            .get(size_key)
            .and_then(Value::as_u64)
            .map(|size| size as f32)
            .filter(|size| (range.min..=range.max).contains(size))
            .unwrap_or(range.default);
        let family = settings.get(family_key).and_then(Value::as_str).and_then(|families| {
            let first = families.split(',').next()?.trim().trim_matches(['"', '\'']).trim();
            (!first.is_empty()).then(|| first.to_owned())
        });
        TerminalTypography { family, size }
    }
}

/// What this device may do with terminals, from its session's grants. Unknown (still loading)
/// grants allow nothing, as `sessionHasScope` reads a pending session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalAccess {
    pub read: bool,
    pub operate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Attached,
    Observing,
}

impl TerminalAccess {
    pub fn of(session: Option<&crate::auth::AuthSessionState>) -> TerminalAccess {
        session.map_or_else(TerminalAccess::default, |session| TerminalAccess {
            read: session.grants(crate::auth::TERMINAL_READ_SCOPE),
            operate: session.grants(crate::auth::TERMINAL_OPERATE_SCOPE),
        })
    }

    /// `attachedTerminalSessionFamily`: operate attaches, read observes, neither follows nothing.
    pub fn mode(self) -> Option<SessionMode> {
        if self.operate {
            Some(SessionMode::Attached)
        } else if self.read {
            Some(SessionMode::Observing)
        } else {
            None
        }
    }
}

/// One shown terminal's lifecycle across what it follows (`createTerminalSession`'s driver).
/// Only leaving the attachment detaches; a transport retry resubscribes the same mode and
/// leaves the lifecycle alone, as the web's retries keep their outer scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionFollow {
    lifecycle: Lifecycle,
    mode: Option<SessionMode>,
}

impl SessionFollow {
    pub fn mode(&self) -> Option<SessionMode> {
        self.mode
    }

    /// Follows what `access` allows now and returns it.
    pub fn follow(&mut self, access: TerminalAccess) -> Option<SessionMode> {
        let next = access.mode();
        if self.mode == Some(SessionMode::Attached) && next != self.mode {
            self.lifecycle.transition(LifecycleEvent::Detached { can_operate: access.operate });
        }
        if next == Some(SessionMode::Observing) {
            self.lifecycle.transition(LifecycleEvent::Observed);
        }
        self.mode = next;
        next
    }

    /// An attached session's status; observers only read.
    pub fn status(&mut self, status: BufferStatus, version: u64) -> Option<LifecycleCommand> {
        if self.mode != Some(SessionMode::Attached) {
            return None;
        }
        self.lifecycle.transition(LifecycleEvent::Status { status, version })
    }

    pub fn reopen_failed(&mut self) {
        self.lifecycle.transition(LifecycleEvent::ReopenFailed);
    }
}

fn terminal_number(terminal_id: &str) -> Option<&str> {
    let lower = terminal_id.to_ascii_lowercase();
    let prefix = if lower.starts_with("terminal-") {
        "terminal-".len()
    } else if lower.starts_with("term-") {
        "term-".len()
    } else {
        return None;
    };
    let rest = &terminal_id[prefix..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let suffix = &rest[digits..];
    if suffix.is_empty() || suffix.strip_prefix('-').is_some_and(is_uuid) {
        Some(&rest[..digits])
    } else {
        None
    }
}

fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `getTerminalLabel`: `term-3` reads "Terminal 3"; anything else is shown as is.
pub fn terminal_label(terminal_id: &str) -> String {
    match terminal_number(terminal_id) {
        Some(number) => format!("Terminal {number}"),
        None => terminal_id.to_owned(),
    }
}

/// `resolveTerminalSessionLabel`: the server's label (the running program) wins when set.
pub fn session_label(terminal_id: &str, summary_label: Option<&str>) -> String {
    match summary_label.map(str::trim).filter(|label| !label.is_empty()) {
        Some(label) => label.to_owned(),
        None => terminal_label(terminal_id),
    }
}

/// `nextTerminalId`: the lowest unused `term-N`. Without a reliable list of the server's
/// sessions the caller passes a unique suffix so it never reuses a session it cannot see.
pub fn next_terminal_id<'a>(existing: impl IntoIterator<Item = &'a str>, unique_suffix: Option<&str>) -> String {
    let used: Vec<&str> = existing.into_iter().filter_map(terminal_number).collect();
    let mut next = 1u32;
    while used.iter().any(|n| n.trim_start_matches('0') == next.to_string()) {
        next += 1;
    }
    match unique_suffix {
        Some(suffix) => format!("term-{next}-{suffix}"),
        None => format!("term-{next}"),
    }
}

/// Where a terminal starts and the environment it gets (`projectScriptCwd`,
/// `projectScriptRuntimeEnv`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchLocation {
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl LaunchLocation {
    pub fn new(workspace_root: &str, worktree_path: Option<&str>) -> Self {
        let mut env = BTreeMap::from([("T3CODE_PROJECT_ROOT".to_owned(), workspace_root.to_owned())]);
        if let Some(worktree) = worktree_path.filter(|path| !path.is_empty()) {
            env.insert("T3CODE_WORKTREE_PATH".to_owned(), worktree.to_owned());
        }
        LaunchLocation {
            cwd: worktree_path.unwrap_or(workspace_root).to_owned(),
            worktree_path: worktree_path.map(str::to_owned),
            env,
        }
    }
}

fn session(thread_id: &str, terminal_id: &str) -> Value {
    json!({ "threadId": thread_id, "terminalId": terminal_id })
}

fn with_grid(mut payload: Value, grid: Option<Grid>) -> Value {
    if let Some(grid) = grid {
        payload["cols"] = json!(grid.cols);
        payload["rows"] = json!(grid.rows);
    }
    payload
}

/// `terminal.open`. The worktree path is sent only when there is one, as `launchTerminal` does.
pub fn open_input(thread_id: &str, terminal_id: &str, location: &LaunchLocation, grid: Option<Grid>) -> Value {
    let mut payload = session(thread_id, terminal_id);
    payload["cwd"] = json!(location.cwd);
    payload["env"] = json!(location.env);
    if let Some(worktree) = &location.worktree_path {
        payload["worktreePath"] = json!(worktree);
    }
    with_grid(payload, grid)
}

/// `terminal.attach`: opens the shell when the server has none. `resume` is set on every
/// resubscription after the first reply so a terminal closed elsewhere stays closed.
pub fn attach_input(thread_id: &str, terminal_id: &str, location: &LaunchLocation, grid: Option<Grid>, resume: bool) -> Value {
    let mut payload = session(thread_id, terminal_id);
    payload["cwd"] = json!(location.cwd);
    payload["worktreePath"] = json!(location.worktree_path);
    payload["env"] = json!(location.env);
    if resume {
        payload["resume"] = json!(true);
    }
    with_grid(payload, grid)
}

pub fn observe_input(thread_id: &str, terminal_id: &str) -> Value {
    session(thread_id, terminal_id)
}

pub fn write_input(thread_id: &str, terminal_id: &str, data: &str) -> Value {
    let mut payload = session(thread_id, terminal_id);
    payload["data"] = json!(data);
    payload
}

pub fn resize_input(thread_id: &str, terminal_id: &str, grid: Grid) -> Value {
    with_grid(session(thread_id, terminal_id), Some(grid))
}

pub fn clear_input(thread_id: &str, terminal_id: &str) -> Value {
    session(thread_id, terminal_id)
}

pub fn close_input(thread_id: &str, terminal_id: &str, delete_history: bool) -> Value {
    let mut payload = session(thread_id, terminal_id);
    if delete_history {
        payload["deleteHistory"] = json!(true);
    }
    payload
}

/// `clampDrawerHeight` with the window cap of `resolveTerminalDrawerMaxHeight`.
pub fn clamp_drawer_height(height: f32, viewport_height: f32) -> f32 {
    let max = MIN_DRAWER_HEIGHT.max((viewport_height * MAX_DRAWER_HEIGHT_RATIO).floor());
    let height = if height.is_finite() { height } else { DEFAULT_DRAWER_HEIGHT };
    height.round().clamp(MIN_DRAWER_HEIGHT, max)
}

/// `confirmTerminalClose`: the message and detail of the destructive confirmation.
pub fn close_confirmation(labels: &[String]) -> (String, String) {
    match labels {
        [label] => (
            format!("Close terminal \"{label}\"?"),
            "This stops the running process and clears its history.".to_owned(),
        ),
        _ => {
            let quoted: Vec<String> = labels.iter().map(|label| format!("\"{label}\"")).collect();
            (
                format!("Close {} terminals?", labels.len()),
                format!("This stops their running processes and clears their histories: {}.", quoted.join(", ")),
            )
        }
    }
}

/// `writeSystemMessage`: notes the client prints into the terminal itself.
pub fn system_message(message: &str) -> String {
    format!("\r\n[terminal] {message}\r\n")
}

#[cfg(test)]
mod tests;
