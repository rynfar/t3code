//! Project scripts: which one the Run button offers, their keybinding commands, and running them
//! in a thread terminal. Ports `apps/web/src/projectScripts.ts`, `resolveProjectScripts`,
//! `projectScriptMenuLabel` and `projectScriptTerminalInput` (`packages/shared/src/projectScripts.ts`),
//! the launch in `components/chat/useChatTerminalActions.ts` and `terminal/useThreadTerminalActions.ts`
//! (its env from `terminal::LaunchLocation`), and the terminal metadata stream
//! (`TerminalMetadataStreamEvent`, `packages/contracts/src/terminal.ts`) that says which terminals
//! still run a command.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::server_config::ServerSettings;
use crate::terminal::{self, Grid, LaunchLocation};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptIcon {
    Test,
    Lint,
    Configure,
    Build,
    Debug,
    #[default]
    #[serde(other)]
    Play,
}

/// `ProjectScript`. Every field round-trips, since settings writes send whole script lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub icon: ScriptIcon,
    #[serde(default)]
    pub run_on_worktree_create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    /// False holds the agent until a setup script exits.
    #[serde(default, rename = "async", skip_serializing_if = "Option::is_none")]
    pub run_async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

impl ProjectScript {
    pub fn runs_on_settle(&self) -> bool {
        self.run_on_settle == Some(true)
    }
}

/// `primaryProjectScript`: the first script with no setup or settle role.
pub fn primary_project_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts
        .iter()
        .find(|script| !script.run_on_worktree_create && !script.runs_on_settle())
        .or_else(|| scripts.iter().find(|script| !script.runs_on_settle()))
}

/// The Run button's script: the last one run in this project, else the primary one.
pub fn preferred_script<'a>(scripts: &'a [ProjectScript], last_run: Option<&str>) -> Option<&'a ProjectScript> {
    last_run.and_then(|id| scripts.iter().find(|script| script.id == id)).or_else(|| primary_project_script(scripts))
}

/// `MAX_SCRIPT_ID_LENGTH`.
const MAX_SCRIPT_ID_LENGTH: usize = 24;

fn valid_script_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_SCRIPT_ID_LENGTH
        && chars.next().is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// `commandForProjectScript`: `script.<id>.run`, when the id fits the keybinding pattern.
pub fn command_for_project_script(script_id: &str) -> Option<String> {
    valid_script_id(script_id).then(|| format!("script.{script_id}.run"))
}

/// `projectScriptIdFromCommand`.
pub fn project_script_id_from_command(command: &str) -> Option<&str> {
    command.trim().strip_prefix("script.")?.strip_suffix(".run").filter(|id| valid_script_id(id))
}

/// `projectScriptMenuLabel`: the name plus its setup and settle roles.
pub fn menu_label(script: &ProjectScript) -> String {
    let roles: Vec<&str> = [(script.run_on_worktree_create, "setup"), (script.runs_on_settle(), "on settle")]
        .into_iter()
        .filter_map(|(has, role)| has.then_some(role))
        .collect();
    if roles.is_empty() { script.name.clone() } else { format!("{} ({})", script.name, roles.join(", ")) }
}

/// `projectScriptTerminalInput`: the leading space keeps the command out of shell history.
pub fn terminal_input(command: &str) -> String {
    format!(" {command}\r")
}

/// `SCRIPT_TERMINAL_COLS` × `SCRIPT_TERMINAL_ROWS`.
const SCRIPT_TERMINAL_SIZE: (u16, u16) = (120, 30);

/// `resolveProjectScripts`: a project's own override, then the folded defaults, then the legacy
/// per-project override (null means the defaults), then the project's scripts or the defaults.
pub fn resolve_project_scripts(settings: &ServerSettings, project_id: &str, project_scripts: Vec<ProjectScript>) -> Vec<ProjectScript> {
    if let Some(scripts) = settings.project_settings_overrides.get(project_id).and_then(|overrides| overrides.default_project_scripts.as_ref()) {
        return scripts.clone();
    }
    if settings.project_settings_folded {
        return settings.default_project_scripts.clone();
    }
    match settings.project_script_overrides.get(project_id) {
        Some(Some(scripts)) => scripts.clone(),
        Some(None) => settings.default_project_scripts.clone(),
        None if !project_scripts.is_empty() => project_scripts,
        None => settings.default_project_scripts.clone(),
    }
}

/// `terminal.open` for a script run (`launchTerminal`): in the thread's checkout, with the
/// project's runtime env.
pub fn open_terminal(thread_id: &str, terminal_id: &str, project_root: &str, worktree_path: Option<&str>) -> Value {
    let location = LaunchLocation::new(project_root, worktree_path);
    terminal::open_input(thread_id, terminal_id, &location, Some(Grid::new(SCRIPT_TERMINAL_SIZE.0, SCRIPT_TERMINAL_SIZE.1)))
}

pub fn write_terminal(thread_id: &str, terminal_id: &str, data: &str) -> Value {
    json!({ "threadId": thread_id, "terminalId": terminal_id, "data": data })
}

/// `TerminalSummary`, the parts that say whether a command still runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub thread_id: String,
    pub terminal_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub has_running_subprocess: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum TerminalMetadataEvent {
    Snapshot { terminals: Vec<TerminalSummary> },
    Upsert { terminal: TerminalSummary },
    Remove { thread_id: String, terminal_id: String },
    #[serde(other)]
    Unknown,
}

/// Every terminal session the server reports, keyed by thread and terminal.
#[derive(Debug, Clone, Default)]
pub struct Terminals {
    by_key: HashMap<(String, String), TerminalSummary>,
}

impl Terminals {
    pub fn apply(&mut self, event: TerminalMetadataEvent) {
        match event {
            TerminalMetadataEvent::Snapshot { terminals } => {
                self.by_key = terminals.into_iter().map(|terminal| ((terminal.thread_id.clone(), terminal.terminal_id.clone()), terminal)).collect();
            }
            TerminalMetadataEvent::Upsert { terminal } => {
                self.by_key.insert((terminal.thread_id.clone(), terminal.terminal_id.clone()), terminal);
            }
            TerminalMetadataEvent::Remove { thread_id, terminal_id } => {
                self.by_key.remove(&(thread_id, terminal_id));
            }
            TerminalMetadataEvent::Unknown => {}
        }
    }

    fn script_terminals<'a>(&'a self, thread_id: &'a str, script_id: &'a str) -> impl Iterator<Item = &'a TerminalSummary> + 'a {
        self.by_key.values().filter(move |terminal| terminal.thread_id == thread_id && owns_terminal(script_id, &terminal.terminal_id))
    }

    /// Whether a terminal this script launched in the thread is still running a command.
    pub fn script_running(&self, thread_id: &str, script_id: &str) -> bool {
        self.script_terminals(thread_id, script_id).any(|terminal| terminal.has_running_subprocess)
    }

    /// The terminal a new run of the script uses: its idle one, else a fresh one beside it
    /// (`findReusableProjectScriptTerminal` never types into a running command).
    pub fn terminal_for_run(&self, thread_id: &str, script_id: &str, fresh_suffix: &str) -> String {
        let base = script_terminal_id(script_id);
        let busy: Vec<&str> = self.script_terminals(thread_id, script_id).filter(|t| t.has_running_subprocess).map(|t| t.terminal_id.as_str()).collect();
        if !busy.contains(&base.as_str()) {
            return base;
        }
        self.script_terminals(thread_id, script_id)
            .find(|terminal| !terminal.has_running_subprocess && terminal.status != "exited")
            .map(|terminal| terminal.terminal_id.clone())
            .unwrap_or_else(|| format!("{base}:{fresh_suffix}"))
    }
}

/// Script ids never hold a colon, so `script:dev` cannot claim `script:dev-server`'s terminals.
fn script_terminal_id(script_id: &str) -> String {
    format!("script:{script_id}")
}

fn owns_terminal(script_id: &str, terminal_id: &str) -> bool {
    let base = script_terminal_id(script_id);
    terminal_id.strip_prefix(&base).is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn script(id: &str, setup: bool, settle: bool) -> ProjectScript {
        ProjectScript { id: id.into(), name: id.to_uppercase(), command: format!("run {id}"), run_on_worktree_create: setup, run_on_settle: Some(settle), ..Default::default() }
    }

    #[test]
    fn primary_script_skips_setup_and_settle_roles() {
        let scripts = vec![script("setup", true, false), script("settle", false, true), script("dev", false, false)];
        assert_eq!(primary_project_script(&scripts).map(|s| s.id.as_str()), Some("dev"));
        let scripts = vec![script("settle", false, true), script("setup", true, false)];
        assert_eq!(primary_project_script(&scripts).map(|s| s.id.as_str()), Some("setup"));
        assert_eq!(primary_project_script(&[script("settle", false, true)]), None);
        let scripts = vec![script("dev", false, false), script("test", false, false)];
        assert_eq!(preferred_script(&scripts, Some("test")).map(|s| s.id.as_str()), Some("test"));
        assert_eq!(preferred_script(&scripts, Some("gone")).map(|s| s.id.as_str()), Some("dev"));
    }

    #[test]
    fn script_commands_round_trip_and_reject_bad_ids() {
        assert_eq!(command_for_project_script("dev-server").as_deref(), Some("script.dev-server.run"));
        assert_eq!(command_for_project_script("Dev"), None);
        assert_eq!(command_for_project_script("-dev"), None);
        assert_eq!(command_for_project_script(&"a".repeat(25)), None);
        assert_eq!(project_script_id_from_command(" script.dev.run "), Some("dev"));
        assert_eq!(project_script_id_from_command("script..run"), None);
        assert_eq!(project_script_id_from_command("thread.new"), None);
    }

    #[test]
    fn labels_input_and_decoding() {
        assert_eq!(menu_label(&script("dev", false, false)), "DEV");
        assert_eq!(menu_label(&script("both", true, true)), "BOTH (setup, on settle)");
        assert_eq!(terminal_input("bun dev"), " bun dev\r");
        let decoded: ProjectScript = serde_json::from_value(json!({ "id": "t", "name": "Test", "command": "bun test", "icon": "rocket", "runOnWorktreeCreate": false })).expect("decodes");
        assert_eq!(decoded.icon, ScriptIcon::Play, "unknown icons fall back to play");
    }

    #[test]
    fn script_terminals_open_with_the_runtime_env() {
        let local = open_terminal("th", "script:t", "/r", None);
        assert_eq!(local["cols"], 120);
        assert_eq!(local["env"], json!({ "T3CODE_PROJECT_ROOT": "/r" }));
        assert!(local.get("worktreePath").is_none());
        let worktree = open_terminal("th", "script:t", "/r", Some("/wt/a"));
        assert_eq!(worktree["cwd"], "/wt/a");
        assert_eq!(worktree["worktreePath"], "/wt/a");
        assert_eq!(worktree["env"], json!({ "T3CODE_PROJECT_ROOT": "/r", "T3CODE_WORKTREE_PATH": "/wt/a" }));
    }

    fn settings(value: serde_json::Value) -> ServerSettings {
        serde_json::from_value(value).expect("decodes")
    }

    #[test]
    fn effective_scripts_follow_the_web_precedence() {
        let own = vec![script("own", false, false)];
        let ids = |scripts: Vec<ProjectScript>| scripts.into_iter().map(|script| script.id).collect::<Vec<_>>();
        let named = |id: &str| json!([{ "id": id, "name": id, "command": "run" }]);
        let defaults = named("default");
        assert_eq!(ids(resolve_project_scripts(&settings(json!({})), "p1", own.clone())), ["own"]);
        assert_eq!(ids(resolve_project_scripts(&settings(json!({ "defaultProjectScripts": defaults })), "p1", vec![])), ["default"]);
        let legacy = settings(json!({ "defaultProjectScripts": defaults, "projectScriptOverrides": { "p1": named("legacy"), "p2": null } }));
        assert_eq!(ids(resolve_project_scripts(&legacy, "p1", own.clone())), ["legacy"]);
        assert_eq!(ids(resolve_project_scripts(&legacy, "p2", own.clone())), ["default"], "a null override means the defaults");
        let folded = settings(json!({ "defaultProjectScripts": defaults, "projectSettingsFolded": true, "projectScriptOverrides": { "p1": named("legacy") } }));
        assert_eq!(ids(resolve_project_scripts(&folded, "p1", own.clone())), ["default"]);
        let project = settings(json!({ "projectSettingsFolded": true, "projectSettingsOverrides": { "p1": { "defaultProjectScripts": [] } } }));
        assert_eq!(ids(resolve_project_scripts(&project, "p1", own.clone())), Vec::<String>::new(), "a project override wins, even empty");
    }

    fn summary(terminal: &str, running: bool) -> TerminalSummary {
        TerminalSummary { thread_id: "th".into(), terminal_id: terminal.into(), status: "running".into(), has_running_subprocess: running }
    }

    #[test]
    fn terminals_track_running_scripts_and_never_reuse_a_busy_one() {
        let mut terminals = Terminals::default();
        assert_eq!(terminals.terminal_for_run("th", "dev", "x1"), "script:dev");
        terminals.apply(TerminalMetadataEvent::Snapshot { terminals: vec![summary("script:dev", true), summary("script:dev-server", false)] });
        assert!(terminals.script_running("th", "dev"));
        assert!(!terminals.script_running("th", "dev-server"));
        assert!(!terminals.script_running("other", "dev"));
        assert_eq!(terminals.terminal_for_run("th", "dev", "x1"), "script:dev:x1");
        terminals.apply(TerminalMetadataEvent::Upsert { terminal: summary("script:dev:x1", false) });
        assert_eq!(terminals.terminal_for_run("th", "dev", "x2"), "script:dev:x1");
        terminals.apply(TerminalMetadataEvent::Upsert { terminal: summary("script:dev", false) });
        assert!(!terminals.script_running("th", "dev"));
        assert_eq!(terminals.terminal_for_run("th", "dev", "x2"), "script:dev");
        let removed: TerminalMetadataEvent = serde_json::from_value(json!({ "type": "remove", "threadId": "th", "terminalId": "script:dev" })).expect("decodes");
        terminals.apply(removed);
        let unknown: TerminalMetadataEvent = serde_json::from_value(json!({ "type": "future" })).expect("decodes");
        assert_eq!(unknown, TerminalMetadataEvent::Unknown);
    }
}
