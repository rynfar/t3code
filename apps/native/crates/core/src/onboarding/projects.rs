//! Ports `web/src/onboarding/projectImport.logic.ts`, `WelcomeWizard.tsx`'s ImportStep,
//! and `contracts/src/agentSessions.ts`. No provider turns are part of history import.
use crate::{
    shell::ProjectShell,
    sidebar::{
        grouping::normalize_project_path,
        time::{DAY_MS, parse_iso},
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub fn confirmed_project_rejection(failure: &crate::rpc::RpcFailure) -> bool {
    // Schema.Defect serializes service errors as name/message; ProjectOperationError can follow a commit.
    failure.tag.as_deref() == Some("ProjectMutationError") && matches!(failure.error["cause"]["name"].as_str(), Some("ProjectConflictError" | "ProjectNotFoundError"))
}

pub const SCAN: &str = "agentSessions.scan";
pub const IMPORT: &str = "agentSessions.import";
pub const PERMISSION_MESSAGE: &str = "This connection cannot import projects or thread history.";
pub const LIMIT_MESSAGE: &str = "Scan limit reached. Some projects or conversations may be missing.";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub path: String,
    pub title: String,
    pub project_id: Option<String>,
    pub sources: Vec<String>,
    pub thread_count: u64,
    pub last_active_at: Option<String>,
    // Missing git means a repository on old servers; explicit null means another folder.
    #[serde(default = "legacy_git")]
    pub git: Value,
}
fn legacy_git() -> Value {
    json!({"remoteKey":null,"repository":null})
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub imported_count: u64,
    pub skipped_count: u64,
}

pub fn key(environment: &str, path: &str) -> String {
    json!([environment, path]).to_string()
}
pub fn recent(candidate: &Candidate, now: i64) -> bool {
    !candidate.git.is_null() && candidate.thread_count >= 3 && candidate.last_active_at.as_deref().and_then(parse_iso).is_some_and(|at| at >= now - 30 * DAY_MS && at <= now)
}
#[derive(Debug, Clone)]
pub struct Group {
    pub key: String,
    pub label: String,
    pub candidates: Vec<usize>,
    pub thread_count: u64,
    pub last_active_at: Option<String>,
}
pub fn group(candidates: &[Candidate]) -> (Vec<Group>, Vec<usize>) {
    let (mut groups, mut other) = (Vec::<Group>::new(), Vec::new());
    let mut indices = HashMap::new();
    for (index, c) in candidates.iter().enumerate() {
        if c.git.is_null() {
            other.push(index);
            continue;
        }
        let key = c.git["remoteKey"].as_str().map_or_else(|| format!("path:{}", c.path), |remote| format!("remote:{remote}"));
        if let Some(&i) = indices.get(&key) {
            let g: &mut Group = &mut groups[i];
            g.candidates.push(index);
            g.thread_count += c.thread_count;
            g.last_active_at = g.last_active_at.clone().max(c.last_active_at.clone());
        } else {
            indices.insert(key.clone(), groups.len());
            groups.push(Group {
                key,
                label: c.git["repository"].as_str().unwrap_or(&c.title).to_owned(),
                candidates: vec![index],
                thread_count: c.thread_count,
                last_active_at: c.last_active_at.clone(),
            });
        }
    }
    groups.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at).then(a.label.cmp(&b.label)));
    (groups, other)
}
pub fn resolve_project(candidate: &Candidate, environment: &str, snapshot_environment: &str, projects: &[ProjectShell]) -> Option<String> {
    candidate.project_id.clone().or_else(|| {
        (environment == snapshot_environment)
            .then(|| projects.iter().find(|p| normalize_project_path(&p.workspace_root) == normalize_project_path(&candidate.path)).map(|p| p.id.clone()))
            .flatten()
    })
}
pub fn split_path(path: &str) -> (&str, &str) {
    let sep = |c| c == '/' || c == '\\';
    let trimmed = path.trim_end_matches(sep);
    let drive_root = trimmed.len() == 2 && trimmed.as_bytes()[0].is_ascii_alphabetic() && trimmed.ends_with(':') && path.len() > 2;
    let unc_root = path.starts_with(['/', '\\']) && path.chars().take(2).all(sep) && trimmed.split(sep).filter(|s| !s.is_empty()).count() == 2;
    if trimmed.is_empty() || drive_root || unc_root {
        return (path, "");
    }
    trimmed.rfind(sep).map_or((trimmed, ""), |i| (&trimmed[i + 1..], &trimmed[..i + 1]))
}

/// Survives rescans and step changes for one destination; IDs survive ambiguous replies.
#[derive(Debug, Default)]
pub struct Progress {
    pub attempts: HashMap<String, String>,
    pub completed: HashMap<String, String>,
    pub with_history: HashMap<String, String>,
    pub selection: Vec<String>,
    pub imported: u64,
    pub skipped: u64,
    pub failures: usize,
}
#[derive(Debug, Clone, Copy)]
pub enum FinishIntent {
    Import,
    Skip,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Completion {
    Denied,
    Ready(Option<String>),
    Wait(String),
}
impl Progress {
    pub fn begin_attempt(&mut self, selection: Vec<String>) {
        self.selection = selection;
        self.imported = 0;
        self.skipped = 0;
        self.failures = 0;
    }
    pub fn completion(&self, intent: FinishIntent, can_operate: bool, listed: impl Fn(&str) -> bool) -> Completion {
        if matches!(intent, FinishIntent::Import) && !self.selection.is_empty() && !can_operate {
            return Completion::Denied;
        }
        match self.landing() {
            Some(id) if listed(id) => Completion::Ready(Some(id.to_owned())),
            Some(id) if matches!(intent, FinishIntent::Import) => Completion::Wait(id.to_owned()),
            _ => Completion::Ready(None),
        }
    }
    pub fn failed_attempt(&mut self, key: &str, definitive: bool) {
        self.failures += 1;
        if definitive {
            self.attempts.remove(key);
        }
    }
    pub fn record(&mut self, key: String, project: String, result: ImportResult) {
        self.imported += result.imported_count;
        self.skipped += result.skipped_count;
        if result.imported_count > 0 {
            self.with_history.insert(key.clone(), project.clone());
        }
        if result.skipped_count == 0 {
            self.completed.insert(key, project);
        }
    }
    pub fn landing(&self) -> Option<&str> {
        self.selection
            .iter()
            .find_map(|key| self.with_history.get(key))
            .or_else(|| self.selection.iter().find_map(|key| self.completed.get(key)))
            .map(String::as_str)
    }
    pub fn warning(&self) -> Option<String> {
        if self.skipped == 0 && self.failures == 0 {
            return None;
        }
        let imported = self.imported;
        let skipped = self.skipped;
        let threads = |n| if n == 1 { "thread" } else { "threads" };
        Some(if skipped > 0 {
            let prefix = if imported > 0 { format!("Imported {imported} {}. ", threads(imported)) } else { String::new() };
            format!("{prefix}{skipped} {} could not be imported.", threads(skipped))
        } else if imported > 0 {
            format!("Imported {imported} {}. Some thread history could not be imported.", threads(imported))
        } else {
            "Could not import thread history.".to_owned()
        })
    }
}
pub fn default_selection(scan: &Scan, environment: &str, now: i64) -> HashSet<String> {
    scan.candidates.iter().filter(|c| recent(c, now)).map(|c| key(environment, &c.path)).collect()
}
pub fn create_payload(project_id: &str, candidate: &Candidate) -> Value {
    json!({"type":"project.create", "commandId":format!("onboarding:project:create:{project_id}"), "projectId":project_id, "title":candidate.title, "workspaceRoot":candidate.path, "createWorkspaceRootIfMissing":false, "defaultModelSelection":null})
}
pub fn import_payload(project_id: &str, candidate: &Candidate) -> Value {
    json!({"projectId":project_id, "expectedWorkspaceRoot":candidate.path})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemandToken {
    connection: u64,
    generation: u64,
}
#[derive(Debug, Default)]
pub struct Demand {
    generation: u64,
    active: Option<DemandToken>,
}
impl Demand {
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.active = None;
    }
    pub fn begin(&mut self, connection: u64) -> DemandToken {
        self.cancel();
        let token = DemandToken { connection, generation: self.generation };
        self.active = Some(token);
        token
    }
    pub fn accepts(&self, token: DemandToken, connection: Option<u64>, allowed: bool) -> bool {
        self.active == Some(token) && connection == Some(token.connection) && allowed
    }
}

#[cfg(test)]
mod tests;
