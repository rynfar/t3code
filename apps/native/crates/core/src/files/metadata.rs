//! Filesystem metadata: the `filesystem.getMetadata` wire shapes
//! (`packages/contracts/src/filesystem.ts`), the cache and per-thread retention of
//! `packages/client-runtime/src/state/fileMetadata.ts`, and the query path rules of
//! `getFileMetadataQueryAtom` in `apps/web/src/hooks/useFileMetadata.ts`.
//!
//! A thread that reads metadata keeps what it saw, so a file link does not flicker when the
//! shared cache moves on. Positive answers stay for the thread's lifetime; misses still expire,
//! because agents mention paths before they create them (`pingdotgg/t3code#30606`).

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Value, json};

use super::position::{is_absolute_path, is_network_path, resolve_path_link_target, split_file_path_position};

pub const GET_METADATA: &str = "filesystem.getMetadata";
/// `METADATA_STALE_TIME_MS`.
pub const STALE_MS: u64 = 60_000;
/// `METADATA_CACHE_LIMIT`.
pub const CACHE_LIMIT: usize = 1024;
/// `FILESYSTEM_METADATA_BATCH_LIMIT`.
pub const BATCH_LIMIT: usize = 64;
/// Servers stat what they are asked about; longer paths are never asked.
const MAX_QUERY_PATH_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataKind {
    File,
    Directory,
    #[serde(other)]
    Other,
}

/// `FilesystemEntryMetadata`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryMetadata {
    pub kind: MetadataKind,
    #[serde(default)]
    pub byte_length: Option<u64>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

impl EntryMetadata {
    /// A file or directory: what a link may open.
    pub fn is_openable(&self) -> bool {
        matches!(self.kind, MetadataKind::File | MetadataKind::Directory)
    }
}

/// The host path `getFileMetadataQueryAtom` asks about: the link's position dropped, relative
/// paths anchored at `cwd`, normalized for comparison. `None` when it may not be asked: not
/// absolute (nor `~/`), a network path, or too long.
pub fn query_path(path: &str, cwd: Option<&str>) -> Option<String> {
    let host = split_file_path_position(path, "").path;
    let target = match cwd {
        Some(cwd) if !host.starts_with("~/") => resolve_path_link_target(&host, cwd),
        _ => host,
    };
    let target = crate::sidebar::grouping::normalize_project_path(&target);
    let absolute = is_absolute_path(&target) || target.starts_with("~/");
    (absolute && !is_network_path(&target) && target.chars().count() <= MAX_QUERY_PATH_CHARS).then_some(target)
}

pub fn request(paths: &[String]) -> Value {
    json!({ "paths": paths })
}

/// `FilesystemGetMetadataResult`: entries follow request order; `null`, or an entry this build
/// cannot read, means missing.
pub fn decode(value: &Value, count: usize) -> Result<Vec<Option<EntryMetadata>>, String> {
    let entries = value.get("entries").and_then(Value::as_array).ok_or("Metadata reply has no entries")?;
    Ok((0..count).map(|index| entries.get(index).and_then(|entry| serde_json::from_value(entry.clone()).ok())).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cached {
    value: Option<EntryMetadata>,
    expires: u64,
}

/// What the cache knows about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Known(Option<EntryMetadata>),
    /// Ask the server, then [`MetadataCache::remember`] the answer.
    Fetch,
}

#[derive(Debug, Default)]
struct Retained {
    readers: usize,
    entries: HashMap<String, Cached>,
}

/// One server session's metadata. A new session starts a new cache, as the web keys it by
/// `RpcSession`.
#[derive(Debug, Default)]
pub struct MetadataCache {
    /// Path to entry and its write order; the oldest write goes first past the limit.
    entries: HashMap<String, (Cached, u64)>,
    order: BTreeMap<u64, String>,
    next: u64,
    threads: HashMap<String, Retained>,
}

impl MetadataCache {
    /// `retainThread`: a reader of `thread` mounted. Retention lasts while any reader does.
    pub fn retain_thread(&mut self, thread: &str) {
        self.threads.entry(thread.to_owned()).or_default().readers += 1;
    }

    pub fn release_thread(&mut self, thread: &str) {
        if let Some(retained) = self.threads.get_mut(thread) {
            retained.readers = retained.readers.saturating_sub(1);
            if retained.readers == 0 {
                self.threads.remove(thread);
            }
        }
    }

    fn known(&self, path: &str, now: u64) -> Option<&Cached> {
        self.entries.get(path).map(|(cached, _)| cached).filter(|cached| cached.expires > now)
    }

    /// The metadata query's `execute`: what `thread` retained (a miss only until it expires),
    /// else the fresh shared entry, else a fetch.
    pub fn lookup(&mut self, path: &str, thread: Option<&str>, now: u64) -> Lookup {
        if let Some(entry) = thread.and_then(|thread| self.threads.get(thread)).and_then(|retained| retained.entries.get(path))
            && (entry.value.is_some() || entry.expires > now)
        {
            return Lookup::Known(entry.value.clone());
        }
        let Some(cached) = self.known(path, now).cloned() else { return Lookup::Fetch };
        if let Some(retained) = thread.and_then(|thread| self.threads.get_mut(thread)) {
            retained.entries.insert(path.to_owned(), cached.clone());
        }
        Lookup::Known(cached.value)
    }

    /// `remember`: the server's answer for `path`, shared and in every thread that already
    /// retained the path, and in the thread that asked.
    pub fn remember(&mut self, path: &str, value: Option<EntryMetadata>, thread: Option<&str>, now: u64) {
        let entry = Cached { value, expires: now + STALE_MS };
        if let Some((_, order)) = self.entries.remove(path) {
            self.order.remove(&order);
        }
        self.next += 1;
        self.order.insert(self.next, path.to_owned());
        self.entries.insert(path.to_owned(), (entry.clone(), self.next));
        for (key, retained) in &mut self.threads {
            if thread == Some(key.as_str()) || retained.entries.contains_key(path) {
                retained.entries.insert(path.to_owned(), entry.clone());
            }
        }
        if self.entries.len() > CACHE_LIMIT
            && let Some((_, oldest)) = self.order.pop_first()
        {
            self.entries.remove(&oldest);
        }
    }
}
