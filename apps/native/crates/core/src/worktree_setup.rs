//! Worktree setup progress. The snapshot `subscribeWorktreeSetup` streams and the stage labels
//! (`packages/contracts/src/worktreeSetup.ts`); which snapshot shows (`resolveVisibleWorktreeSetup`
//! and `worktreeSetupAgentStarted` in `packages/client-runtime/src/worktreeSetup.ts`,
//! `resolveWorktreeSetupProgress` in `apps/web/src/components/ChatView.logic.ts`, and the held
//! snapshot and send block of `apps/web/src/components/chat/useWorktreeSetup.ts`); and the card's
//! copy and layout rules (`apps/web/src/components/chat/WorktreeSetupCard.tsx`).

use std::sync::Arc;

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::timeline::{format_duration, parse_iso_ms};

pub mod method {
    pub const SUBSCRIBE: &str = "subscribeWorktreeSetup";
    pub const CANCEL: &str = "worktreeSetup.cancel";
}

/// `ChatView`'s `sendDisabledReason` while setup holds the send back.
pub const SEND_BLOCKED: &str = "Preparing worktree";
/// The working row's label while the worktree is prepared (`WorkingTimelineRow`).
pub const PREPARING_LABEL: &str = "Setting up worktree…";
pub const BACKGROUND_SETUP: &str = "Background setup";
pub const DETAILS: &str = "Details";
pub const OPEN_TERMINAL: &str = "Open terminal";
pub const WORK_LOCALLY: &str = "Work locally";
pub const CANCEL: &str = "Cancel";
/// `OUTPUT_TAIL_LINES`: the setup script's last lines the card shows.
pub const OUTPUT_TAIL_LINES: usize = 4;

/// `WorktreeSetupSubscribeInput` and `WorktreeSetupCancelInput`.
pub fn thread_request(thread_id: &str) -> Value {
    json!({ "threadId": thread_id })
}

/// `WorktreeSetupCancelResult.cancelled`; anything else is not a cancellation.
pub fn cancelled(result: &Value) -> bool {
    result.get("cancelled").and_then(Value::as_bool) == Some(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
    #[default]
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
    /// A status this client does not know draws as pending.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
    /// A phase this client does not know is never shown.
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub id: StageId,
    #[serde(default)]
    pub status: StageStatus,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub percent: Option<f64>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScript {
    pub name: String,
    pub command: String,
    pub terminal_id: String,
}

/// `WorktreeSetupSnapshot`. Stages with an id this client does not know, or that do not decode,
/// are left out, so they never fail the frame.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    pub thread_id: String,
    pub phase: Phase,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub worktree_path: Option<String>,
    #[serde(deserialize_with = "lenient_script")]
    pub setup_script: Option<SetupScript>,
    #[serde(deserialize_with = "known_stages")]
    pub stages: Vec<Stage>,
    pub error: Option<String>,
    pub sequence: u64,
}

fn known_stages<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Stage>, D::Error> {
    let raw = Option::<Vec<Value>>::deserialize(d)?.unwrap_or_default();
    Ok(raw.into_iter().filter_map(|value| serde_json::from_value::<Stage>(value).ok()).filter(|stage| stage.id != StageId::Unknown).collect())
}

fn lenient_script<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SetupScript>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(|value| serde_json::from_value(value).ok()))
}

/// One `WorktreeSetupStreamEvent`: the thread's snapshot, or `None` once the server forgets it
/// (30 seconds after it settles) or never had one.
pub fn decode_event(value: Value) -> Result<Option<Snapshot>, serde_json::Error> {
    serde_json::from_value(value)
}

impl Snapshot {
    pub fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    pub fn stage(&self, id: StageId) -> Option<&Stage> {
        self.stages.iter().find(|stage| stage.id == id)
    }

    fn has_failed_stage(&self) -> bool {
        self.stages.iter().any(|stage| stage.status == StageStatus::Failed)
    }
}

/// `worktreeSetupAgentStarted`.
pub fn agent_started(snapshot: &Snapshot) -> bool {
    snapshot.stages.iter().any(|stage| stage.id == StageId::Agent && stage.status == StageStatus::Done)
}

/// What one thread's subscription has delivered: the newest event, and the last snapshot, held
/// after the server drops it (or the subscription stops) until settled activity reaches the
/// projection (`useWorktreeSetup`'s `heldSnapshot`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    pub latest: Option<Arc<Snapshot>>,
    pub held: Option<Arc<Snapshot>>,
}

impl Feed {
    pub fn apply(&mut self, event: Option<Arc<Snapshot>>) {
        if let Some(latest) = &event {
            let keep = self.held.as_ref().is_some_and(|held| held.thread_id == latest.thread_id && held.sequence > latest.sequence);
            if !keep {
                self.held = Some(latest.clone());
            }
        }
        self.latest = event;
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub snapshot: Option<Arc<Snapshot>>,
    /// `isPreparingWorktree`: the working row says the worktree is being set up.
    pub preparing: bool,
}

/// `resolveWorktreeSetupProgress`: the newer of the streamed and held snapshot for `thread_id`.
pub fn progress(thread_id: &str, local_preparing: bool, run_status: Option<&str>, feed: &Feed) -> Progress {
    let latest = feed.latest.as_ref().filter(|snapshot| snapshot.thread_id == thread_id);
    let held = feed.held.as_ref().filter(|snapshot| snapshot.thread_id == thread_id);
    let snapshot = match (latest, held) {
        (Some(latest), Some(held)) if latest.sequence < held.sequence => Some(held),
        (Some(latest), _) => Some(latest),
        (None, held) => held,
    }
    .cloned();
    let preparing = local_preparing
        || run_status == Some("preparing")
        || snapshot.as_ref().is_some_and(|snapshot| snapshot.running() && !agent_started(snapshot));
    Progress { snapshot, preparing }
}

/// `resolveVisibleWorktreeSetup`: a running setup always shows; a settled one until a follow-up
/// is sent, and a clean one only until the turn starts. A phase this client does not know never
/// shows.
pub fn resolve_visible(live: Option<&Arc<Snapshot>>, recorded: Option<&Arc<Snapshot>>, turn_started: bool, follow_up_sent: bool) -> Option<Arc<Snapshot>> {
    let snapshot = match (live, recorded) {
        (Some(live), Some(recorded)) if live.sequence < recorded.sequence => recorded,
        (Some(live), _) => live,
        (None, Some(recorded)) => recorded,
        (None, None) => return None,
    };
    match snapshot.phase {
        Phase::Running => Some(snapshot.clone()),
        Phase::Unknown => None,
        _ if follow_up_sent => None,
        Phase::Done if turn_started && !snapshot.has_failed_stage() => None,
        _ => Some(snapshot.clone()),
    }
}

/// `useWorktreeSetup`'s query condition: the open thread has a worktree or its run is being
/// prepared. A subscription the server failed (authorization) stays down until the session is
/// replaced, rather than retrying on every update.
pub fn wants_subscription(has_worktree: bool, run_status: Option<&str>, failed: bool) -> bool {
    !failed && (has_worktree || run_status == Some("preparing"))
}

/// `sendQueuesBehindSetup`: a server thread whose run waits on preparation queues the send.
pub fn send_queues_behind_setup(run_status: Option<&str>) -> bool {
    matches!(run_status, Some("preparing" | "starting"))
}

/// `useWorktreeSetup`'s `blocksSend`: setup holds the send back until the agent takes over.
pub fn blocks_send(visible: Option<&Snapshot>, preparing: bool, queues_behind_setup: bool) -> bool {
    !queues_behind_setup
        && match visible {
            Some(snapshot) => snapshot.running() && !agent_started(snapshot),
            None => preparing,
        }
}

/// `backgroundWorktreeSetup`: the agent took over while the setup script still runs, so the
/// working row carries a chip instead of the card.
pub fn background(visible: Option<&Arc<Snapshot>>, turn_started: bool) -> Option<Arc<Snapshot>> {
    visible.filter(|snapshot| snapshot.running() && agent_started(snapshot) && turn_started).cloned()
}

/// `worktreeSetupStageLabel`.
pub fn stage_label(id: StageId) -> &'static str {
    match id {
        StageId::Fetch => "Fetch base branch",
        StageId::Checkout => "Check out files",
        StageId::Submodules => "Init submodules",
        StageId::SetupScript => "Run setup script",
        StageId::Agent => "Start agent",
        StageId::Unknown => "",
    }
}

/// The stage's label, with the setup script named when the server knows it.
pub fn stage_display_label<'a>(stage: &Stage, script_name: Option<&'a str>) -> &'a str {
    match script_name {
        Some(name) if stage.id == StageId::SetupScript => name,
        _ => stage_label(stage.id),
    }
}

/// `headerLabel`.
pub fn header_label(snapshot: &Snapshot) -> &'static str {
    match snapshot.phase {
        Phase::Running => PREPARING_LABEL,
        Phase::Done if snapshot.has_failed_stage() => "Worktree ready, setup script failed",
        Phase::Done => "Worktree ready",
        Phase::Failed => "Worktree setup failed",
        Phase::Cancelled => "Worktree setup cancelled",
        Phase::Unknown => "",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Warning,
    Destructive,
}

/// `SetupHeaderRow`'s text color.
pub fn header_tone(snapshot: &Snapshot) -> Tone {
    match snapshot.phase {
        Phase::Failed => Tone::Destructive,
        Phase::Done if snapshot.has_failed_stage() => Tone::Warning,
        _ => Tone::Muted,
    }
}

/// `CollapsedSummaryRow`'s status: failed when setup failed, was cancelled, or its script failed.
pub fn summary_status(snapshot: &Snapshot) -> StageStatus {
    let script_failed = snapshot.stage(StageId::SetupScript).is_some_and(|stage| stage.status == StageStatus::Failed);
    if matches!(snapshot.phase, Phase::Failed | Phase::Cancelled) || script_failed { StageStatus::Failed } else { StageStatus::Done }
}

/// The text after a stage's label: its detail, "skipped", or the checkout's percent.
pub fn stage_trailing(stage: &Stage) -> Option<String> {
    match stage.status {
        StageStatus::Pending | StageStatus::Unknown => None,
        StageStatus::Skipped => Some(stage.detail.clone().unwrap_or_else(|| "skipped".to_owned())),
        StageStatus::Running if stage.id == StageId::Checkout && stage.percent.is_some() => {
            let percent = stage.percent.unwrap_or_default();
            Some(if percent.fract() == 0. { format!("{}%", percent as i64) } else { format!("{percent}%") })
        }
        _ => stage.detail.clone(),
    }
}

/// When a stage's elapsed time shows, and whether it is still counting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elapsed {
    /// Settled: `startedAt` to `endedAt`, formatted.
    Fixed(String),
    /// Counting from `startedAt` once a second while setup runs.
    Live(String),
}

/// `Elapsed`: nothing without a start; a settled span once it ended; otherwise a live count only
/// while setup runs. A stage interrupted by setup ending stops with it.
pub fn elapsed(started_at: Option<&str>, ended_at: Option<&str>, live: bool) -> Option<Elapsed> {
    let started_at = started_at?;
    match ended_at {
        Some(ended_at) => Some(Elapsed::Fixed(format_span(started_at, parse_iso_ms(ended_at)?)?)),
        None => live.then(|| Elapsed::Live(started_at.to_owned())),
    }
}

/// The trailing duration a stage shows; none while pending or skipped.
pub fn stage_elapsed(stage: &Stage, snapshot: &Snapshot) -> Option<Elapsed> {
    if matches!(stage.status, StageStatus::Pending | StageStatus::Skipped | StageStatus::Unknown) {
        return None;
    }
    elapsed(stage.started_at.as_deref(), stage.ended_at.as_deref().or(snapshot.ended_at.as_deref()), snapshot.running())
}

/// `formatDuration(elapsedMs(startedAt, endMs))`.
pub fn format_span(started_at: &str, end_ms: i64) -> Option<String> {
    let start = parse_iso_ms(started_at)?;
    Some(format_duration((end_ms - start).max(0) as f64))
}

/// A live count at `now_ms`, read on the shared second tick (`secondTick`: floored to the second).
pub fn live_label(started_at: &str, now_ms: i64) -> Option<String> {
    format_span(started_at, now_ms.div_euclid(1_000) * 1_000)
}

/// Which parts of the card show (`WorktreeSetupCard`). `embedded` is the settled card once the
/// agent took over, or the background chip's popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub show_header: bool,
    pub collapsed: bool,
    /// The setup script's last lines, under its stage.
    pub show_tail: bool,
    /// "Open terminal": the setup script has started.
    pub script_started: bool,
    pub show_error: bool,
}

pub fn layout(snapshot: &Snapshot, embedded: bool) -> Layout {
    let running = snapshot.running();
    let script = snapshot.stage(StageId::SetupScript);
    Layout {
        show_header: !embedded && !running && snapshot.phase != Phase::Done,
        collapsed: embedded && !running,
        show_tail: script.is_some_and(|stage| stage.status == StageStatus::Failed || (stage.status == StageStatus::Running && !stage.tail.is_empty())),
        script_started: script.is_some_and(|stage| !matches!(stage.status, StageStatus::Pending | StageStatus::Unknown)),
        show_error: snapshot.phase == Phase::Failed && snapshot.error.as_deref().is_some_and(|error| !error.is_empty()),
    }
}

/// `OutputTail`: always four slots, the newest lines at the bottom, empty slots blank.
pub fn tail_rows(lines: &[String]) -> [&str; OUTPUT_TAIL_LINES] {
    let mut rows = [""; OUTPUT_TAIL_LINES];
    let start = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    let offset = OUTPUT_TAIL_LINES - (lines.len() - start);
    for (slot, line) in lines[start..].iter().enumerate() {
        rows[offset + slot] = line;
    }
    rows
}

/// `SetupDetails`: label and value pairs, in order, for what the server reported.
pub fn details(snapshot: &Snapshot) -> Vec<(&'static str, &str)> {
    [
        ("Branch", snapshot.branch.as_deref()),
        ("Base", snapshot.base_ref.as_deref()),
        ("Path", snapshot.worktree_path.as_deref()),
        ("Setup", snapshot.setup_script.as_ref().map(|script| script.command.as_str())),
    ]
    .into_iter()
    .filter_map(|(label, value)| Some((label, value?)))
    .collect()
}

/// A thread the composer launched from its draft into a new worktree, on one connection, and
/// the launch it sent, which "Work locally" may resend.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftLaunch {
    pub thread_id: String,
    pub connection: u64,
    pub launch: Value,
}

impl DraftLaunch {
    /// The resend in the project checkout, only on the connection that launched it: another
    /// environment has neither the project nor the message.
    pub fn relaunch(&self, connection: Option<u64>, command_id: &str, thread_id: &str, message_id: &str) -> Option<Value> {
        (connection == Some(self.connection)).then(|| local_relaunch(&self.launch, command_id, thread_id, message_id))
    }
}

/// "Work locally" relaunches the draft's message in the project checkout (`onWorktreeSetupWorkLocally`
/// cancels, switches the draft to local, and resends): `launch` with fresh ids and a root workspace.
pub fn local_relaunch(launch: &Value, command_id: &str, thread_id: &str, message_id: &str) -> Value {
    let mut payload = launch.clone();
    payload["commandId"] = json!(command_id);
    payload["threadId"] = json!(thread_id);
    payload["workspaceStrategy"] = json!({ "type": "root" });
    if payload.get("initialMessage").is_some_and(Value::is_object) {
        payload["initialMessage"]["messageId"] = json!(message_id);
    }
    payload
}

#[cfg(test)]
mod tests;
