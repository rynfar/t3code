//! Worktrees at environment scope: `WorktreesSettings.tsx`, `BranchNamingSettings.tsx`,
//! `WorktreeSubmodulesRow` (`ProjectDefaultsSettings.tsx`), and `WorktreesDirectoryRow` and
//! `WorktreeCleanupSection` (`StorageSettings.tsx`), with `resolveWorktreeCleanup` from
//! `shared/src/projectSettings.ts`. Keys and defaults follow `contracts/src/settings.ts`.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{ServerSettingChange, lenient_or};

/// `StorageRetentionDays`' ceiling, shared by every retention field.
pub const MAX_RETENTION_DAYS: u32 = 3650;
/// What a retention switch turns on to (`RetentionControl`).
pub const DEFAULT_RETENTION_DAYS: u32 = 8;
/// `DEFAULT_SERVER_SETTINGS.branchNamePrefix`.
pub const DEFAULT_BRANCH_PREFIX: &str = "t3";

/// `WorktreeSubmodules` with `WORKTREE_SUBMODULES_LABELS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submodules {
    Recursive,
    TopLevel,
    None,
}

impl Submodules {
    pub const ALL: [Submodules; 3] = [Submodules::Recursive, Submodules::TopLevel, Submodules::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Submodules::Recursive => "recursive",
            Submodules::TopLevel => "top-level",
            Submodules::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Submodules> {
        Submodules::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Submodules::Recursive => "Recursive",
            Submodules::TopLevel => "Top level only",
            Submodules::None => "Skip",
        }
    }
}

/// `BranchNamingMode` with `BranchNamingSettings`' labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BranchNaming {
    #[default]
    Static,
    Semantic,
    Custom,
}

impl BranchNaming {
    pub const ALL: [BranchNaming; 3] = [BranchNaming::Static, BranchNaming::Semantic, BranchNaming::Custom];

    pub fn as_str(self) -> &'static str {
        match self {
            BranchNaming::Static => "static",
            BranchNaming::Semantic => "semantic",
            BranchNaming::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<BranchNaming> {
        BranchNaming::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            BranchNaming::Static => "Static prefix",
            BranchNaming::Semantic => "Semantic prefix",
            BranchNaming::Custom => "Custom instructions",
        }
    }
}

/// `WorktreeKeepWhen` with `KEEP_WHEN_LABELS` and the row's description for each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeepWhen {
    AnyLocalFiles,
    #[default]
    UncommittedChanges,
    TrackedChanges,
}

impl KeepWhen {
    pub const ALL: [KeepWhen; 3] = [KeepWhen::AnyLocalFiles, KeepWhen::UncommittedChanges, KeepWhen::TrackedChanges];

    pub fn as_str(self) -> &'static str {
        match self {
            KeepWhen::AnyLocalFiles => "any-local-files",
            KeepWhen::UncommittedChanges => "uncommitted-changes",
            KeepWhen::TrackedChanges => "tracked-changes",
        }
    }

    pub fn parse(value: &str) -> Option<KeepWhen> {
        KeepWhen::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            KeepWhen::AnyLocalFiles => "Any local files",
            KeepWhen::UncommittedChanges => "Uncommitted changes",
            KeepWhen::TrackedChanges => "Edited tracked files",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            KeepWhen::AnyLocalFiles => "Includes ignored files, except node_modules.",
            KeepWhen::TrackedChanges => "Deletes untracked and ignored files, including .env.",
            KeepWhen::UncommittedChanges => "Keeps edits and untracked files. Deletes ignored files, including .env.",
        }
    }
}

/// `WorktreeCleanupRules`, also the worktree half of `StorageCleanupSettings`. Each field falls
/// back to its default alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CleanupRules {
    #[serde(deserialize_with = "keep_when")]
    pub worktree_keep_when: KeepWhen,
    #[serde(deserialize_with = "retention_days")]
    pub worktree_after_days: Option<u32>,
    #[serde(deserialize_with = "settled_days")]
    pub worktree_settled_after_days: Option<u32>,
    #[serde(deserialize_with = "lenient_or")]
    pub worktree_on_merge: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub worktree_on_delete: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub worktree_unchanged: bool,
}

/// The environment-wide `worktreeCleanup` policy; `None` uses `storageCleanup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Off,
    Custom(CleanupRules),
}

/// The `ServerSettings` keys the Worktrees page edits, besides `newWorktreesStartFromOrigin`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorktreeSettings {
    #[serde(deserialize_with = "trimmed")]
    pub worktrees_directory: String,
    /// `None` inherits the project's `t3.json`, then recursive.
    #[serde(deserialize_with = "submodules")]
    pub worktree_submodules: Option<Submodules>,
    #[serde(deserialize_with = "branch_naming")]
    pub branch_naming_mode: BranchNaming,
    #[serde(deserialize_with = "branch_prefix")]
    pub branch_name_prefix: String,
    #[serde(deserialize_with = "trimmed")]
    pub branch_name_instructions: String,
    #[serde(deserialize_with = "lenient_or")]
    pub storage_cleanup: CleanupRules,
    #[serde(deserialize_with = "policy")]
    pub worktree_cleanup: Option<CleanupPolicy>,
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        WorktreeSettings {
            worktrees_directory: String::new(),
            worktree_submodules: None,
            branch_naming_mode: BranchNaming::default(),
            branch_name_prefix: DEFAULT_BRANCH_PREFIX.to_owned(),
            branch_name_instructions: String::new(),
            storage_cleanup: CleanupRules::default(),
            worktree_cleanup: None,
        }
    }
}

impl WorktreeSettings {
    /// `resolveWorktreeCleanup(settings, null)`: what the cleanup rows show.
    pub fn cleanup(&self) -> CleanupRules {
        match self.worktree_cleanup {
            Some(CleanupPolicy::Custom(rules)) => rules,
            Some(CleanupPolicy::Off) => CleanupRules { worktree_keep_when: self.storage_cleanup.worktree_keep_when, ..CleanupRules::default() },
            None => self.storage_cleanup,
        }
    }

    /// The submodules mode new worktrees use when neither the project nor its `t3.json` set one.
    pub fn effective_submodules(&self) -> Submodules {
        self.worktree_submodules.unwrap_or(Submodules::Recursive)
    }
}

/// One cleanup rule the page edits, saved under `storageCleanup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupRule {
    OnDelete(bool),
    OnMerge(bool),
    AfterDays(Option<u32>),
    SettledAfterDays(Option<u32>),
    Unchanged(bool),
    KeepWhen(KeepWhen),
}

/// One Worktrees page edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeChange {
    Directory(String),
    StartFromOrigin(bool),
    Submodules(Option<Submodules>),
    BranchNaming(BranchNaming),
    BranchPrefix(String),
    BranchInstructions(String),
    Cleanup(CleanupRule),
}

/// `RetentionControl`'s commit: rounded and held inside the field's bounds.
pub fn clamp_retention(days: f64, minimum: u32) -> u32 {
    (days.round().max(0.) as u32).clamp(minimum, MAX_RETENTION_DAYS)
}

impl WorktreeChange {
    fn patch(&self) -> Value {
        match self {
            WorktreeChange::Directory(path) => json!({ "worktreesDirectory": path.trim() }),
            WorktreeChange::StartFromOrigin(on) => json!({ "newWorktreesStartFromOrigin": on }),
            WorktreeChange::Submodules(mode) => json!({ "worktreeSubmodules": mode.map(Submodules::as_str) }),
            WorktreeChange::BranchNaming(mode) => json!({ "branchNamingMode": mode.as_str() }),
            WorktreeChange::BranchPrefix(prefix) => json!({ "branchNamePrefix": prefix.trim() }),
            WorktreeChange::BranchInstructions(text) => json!({ "branchNameInstructions": text.trim() }),
            WorktreeChange::Cleanup(rule) => {
                let (key, value) = match *rule {
                    CleanupRule::OnDelete(on) => ("worktreeOnDelete", json!(on)),
                    CleanupRule::OnMerge(on) => ("worktreeOnMerge", json!(on)),
                    CleanupRule::AfterDays(days) => ("worktreeAfterDays", json!(days.map(|days| days.clamp(1, MAX_RETENTION_DAYS)))),
                    CleanupRule::SettledAfterDays(days) => ("worktreeSettledAfterDays", json!(days.map(|days| days.min(MAX_RETENTION_DAYS)))),
                    CleanupRule::Unchanged(on) => ("worktreeUnchanged", json!(on)),
                    CleanupRule::KeepWhen(mode) => ("worktreeKeepWhen", json!(mode.as_str())),
                };
                json!({ "storageCleanup": { key: value } })
            }
        }
    }

    pub fn into_server(self) -> ServerSettingChange {
        ServerSettingChange::Prepared(json!({ "patch": self.patch() }))
    }
}

fn trimmed<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Value::deserialize(d)?.as_str().map(|value| value.trim().to_owned()).unwrap_or_default())
}

fn branch_prefix<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Value::deserialize(d)?.as_str().map_or_else(|| DEFAULT_BRANCH_PREFIX.to_owned(), |value| value.trim().to_owned()))
}

fn submodules<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Submodules>, D::Error> {
    Ok(Value::deserialize(d)?.as_str().and_then(Submodules::parse))
}

fn branch_naming<'de, D: Deserializer<'de>>(d: D) -> Result<BranchNaming, D::Error> {
    Ok(Value::deserialize(d)?.as_str().and_then(BranchNaming::parse).unwrap_or_default())
}

fn keep_when<'de, D: Deserializer<'de>>(d: D) -> Result<KeepWhen, D::Error> {
    Ok(Value::deserialize(d)?.as_str().and_then(KeepWhen::parse).unwrap_or_default())
}

/// A whole number of days in `minimum..=3650`, else off.
fn days_from(value: &Value, minimum: u32) -> Option<u32> {
    value.as_u64().and_then(|days| u32::try_from(days).ok()).filter(|days| (minimum..=MAX_RETENTION_DAYS).contains(days))
}

fn retention_days<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(days_from(&Value::deserialize(d)?, 1))
}

fn settled_days<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(days_from(&Value::deserialize(d)?, 0))
}

fn policy<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CleanupPolicy>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(match value.get("mode").and_then(Value::as_str) {
        Some("off") => Some(CleanupPolicy::Off),
        Some("custom") => Some(CleanupPolicy::Custom(value.get("rules").cloned().and_then(|rules| serde_json::from_value(rules).ok()).unwrap_or_default())),
        _ => None,
    })
}

#[cfg(test)]
mod tests;
