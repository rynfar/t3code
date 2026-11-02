//! Rows a shell leaves to on-demand reads (`omitIdleSubagents`, `omitOlderSettled`), merged behind
//! the shell's own rows. Ports `client-runtime/src/state/threadFamilies.ts`: a family is every
//! active thread sharing a `lineage.rootThreadId`, and older-settled shelf pages merge the same way.
//! Reads belong to one shell generation and are never persisted.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::shell::{ShellState, ThreadShell};

pub const GET_THREAD_FAMILY: &str = "orchestration.getThreadFamily";

/// `OrchestrationV2ThreadFamily`; an older-settled page and a search result's older rows decode
/// the same way (the search result's keys are optional: `snapshotSequence ?? 0`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyRead {
    #[serde(default)]
    pub snapshot_sequence: u64,
    #[serde(default)]
    pub threads: Vec<ThreadShell>,
}

pub fn family_request(thread_id: &str) -> Value {
    json!({ "threadId": thread_id })
}

/// `threadFamilyRowsMissingFromShell`. A shell row is at least as new as any read, since shell
/// deltas carry every change; a read from before the shell removed a thread is stale.
pub fn rows_missing_from_shell<'a>(
    shell_threads: &[ThreadShell],
    reads: impl IntoIterator<Item = &'a FamilyRead>,
    removed_at: &HashMap<String, u64>,
) -> Vec<&'a ThreadShell> {
    let mut present: HashSet<&str> = shell_threads.iter().map(|thread| thread.id.as_str()).collect();
    let mut missing = Vec::new();
    for read in reads {
        for thread in &read.threads {
            if present.contains(thread.id.as_str()) {
                continue;
            }
            if removed_at.get(&thread.id).is_some_and(|&sequence| sequence > read.snapshot_sequence) {
                continue;
            }
            present.insert(thread.id.as_str());
            missing.push(thread);
        }
    }
    missing
}

/// Reads merged behind the shell, by key, in mount order (`mountRows`). Each read keeps the shell
/// generation it was made for; a new authoritative snapshot leaves them out until read again.
#[derive(Debug, Clone, Default)]
pub struct MountedReads {
    mounts: Vec<(String, u64, Vec<FamilyRead>)>,
}

impl MountedReads {
    /// Mounts (or replaces) the reads under `key`, made for shell `generation`.
    pub fn mount(&mut self, key: &str, generation: u64, reads: Vec<FamilyRead>) {
        match self.mounts.iter_mut().find(|(existing, ..)| existing == key) {
            Some(entry) => *entry = (key.to_owned(), generation, reads),
            None => self.mounts.push((key.to_owned(), generation, reads)),
        }
    }

    pub fn unmount(&mut self, key: &str) {
        self.mounts.retain(|(existing, ..)| existing != key);
    }

    pub fn clear(&mut self) {
        self.mounts.clear();
    }

    /// The shell generation the reads under `key` were made for.
    pub fn generation_of(&self, key: &str) -> Option<u64> {
        self.mounts.iter().find(|(existing, ..)| existing == key).map(|(_, generation, _)| *generation)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.mounts.iter().map(|(key, ..)| key.as_str())
    }

    /// Current-generation reads at or after the snapshot's own cut (`snapshotAtom` in
    /// `threadFamilies.ts`).
    fn current_reads<'a>(&'a self, shell: &'a ShellState) -> impl Iterator<Item = &'a FamilyRead> {
        let cut = shell.snapshot.as_ref().and_then(|snapshot| snapshot.older_settled.as_ref()).map_or(0, |older| older.as_of_sequence);
        self.mounts
            .iter()
            .filter(move |(_, generation, _)| *generation == shell.generation)
            .flat_map(|(_, _, reads)| reads)
            .filter(move |read| read.snapshot_sequence >= cut)
    }

    /// Rows the shell lacks, from the current reads.
    pub fn extras<'a>(&'a self, shell: &'a ShellState) -> Vec<&'a ThreadShell> {
        let Some(snapshot) = &shell.snapshot else { return Vec::new() };
        rows_missing_from_shell(&snapshot.threads, self.current_reads(shell), &shell.removed_at)
    }

    /// A thread from the shell, else from a mounted read: the row `extras` would merge for it.
    pub fn thread<'a>(&'a self, shell: &'a ShellState, id: &str) -> Option<&'a ThreadShell> {
        if let Some(thread) = shell.thread(id) {
            return Some(thread);
        }
        shell.snapshot.as_ref()?;
        let removed = shell.removed_at.get(id).copied();
        self.current_reads(shell)
            .filter(|read| removed.is_none_or(|sequence| sequence <= read.snapshot_sequence))
            .find_map(|read| read.threads.iter().find(|thread| thread.id == id))
    }
}

pub fn family_key(thread_id: &str) -> String {
    format!("family:{thread_id}")
}

/// Whether the open thread needs its family read: the shell itself leaves it out (an idle
/// subagent, an older settled thread, or a row a new snapshot cut while it was open) and no read
/// for this generation is mounted or `requested`. Other mounts (a page, a search) can let go of it.
pub fn family_read_due(shell: &ShellState, mounts: &MountedReads, thread_id: &str, requested: Option<u64>) -> bool {
    shell.snapshot.is_some()
        && shell.thread(thread_id).is_none()
        && requested != Some(shell.generation)
        && mounts.generation_of(&family_key(thread_id)) != Some(shell.generation)
}

#[cfg(test)]
mod tests;
