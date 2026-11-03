//! What the app keeps on this device, in the OS config dir: saved environments (owner-only, they
//! hold credentials), the last open thread, client settings, and UI state.
//!
//! Saved environments mirror the web's saved connection catalog (`client-runtime/src/connection/
//! catalog.ts`, `profileStore.ts`) in what this client uses: each paired server with its label,
//! address, and credential. One is connected at a time; the list is ready for more. The single
//! `connection.json` of earlier builds is still read when no `environments.json` exists.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

use crate::auth::{Credentials, Paired};

static HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Points every read and write at `home` for the rest of the process, ahead of
/// `T3_NATIVE_HOME`. Tools that must not touch the caller's saved state call it before anything
/// loads. Only the first call takes effect.
pub fn use_home(home: PathBuf) {
    let _ = HOME_OVERRIDE.set(home);
}

pub(crate) fn base() -> Option<PathBuf> {
    HOME_OVERRIDE
        .get()
        .cloned()
        .or_else(|| std::env::var_os("T3_NATIVE_HOME").map(PathBuf::from))
        .or_else(|| dirs::config_dir().map(|dir| dir.join("T3 Code Native")))
}

const ENVIRONMENTS: &str = "environments.json";
/// The writer queue's key for `ENVIRONMENTS`.
const ENVIRONMENTS_KEY: &str = "environments";
/// The single saved connection of earlier builds.
const LEGACY_CONNECTION: &str = "connection.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedEnvironment {
    /// This device's key for the entry; stable across re-pairing.
    pub id: String,
    /// The server's own `environmentId`, once known.
    #[serde(default)]
    pub environment_id: Option<String>,
    pub label: String,
    pub http_base: String,
    /// `None` once the server rejected it: the entry stays until it is paired again or removed.
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
    /// Paired with this Mac's own T3 Code through `t3 pair`, which can pair it again.
    #[serde(default)]
    pub local: bool,
    /// Reached over an SSH tunnel the app manages (`SshConnectionProfile`). `http_base` is then
    /// the tunnel's local address for the current session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<t3_core::ssh::SshTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<t3_core::relay::Environment>,
}

impl SavedEnvironment {
    /// How the environment is reached, as its row shows it: `SSH user@host` over SSH, else its
    /// address (`environmentTransportLabel`).
    pub fn address(&self) -> String {
        if self.relay.is_some() { return "T3 Connect".into(); }
        match &self.ssh {
            Some(target) => format!("SSH {}", t3_core::ssh::format_target(target)),
            None => self.http_base.clone(),
        }
    }

    pub fn credentials(&self) -> Option<Credentials> {
        let access_token = self.access_token.clone()?;
        Some(Credentials { http_base: self.http_base.clone(), access_token, relay: None })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedEnvironments {
    /// The environment to connect at launch; `None` after an explicit disconnect.
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub environments: Vec<SavedEnvironment>,
}

/// `host[:port]` of an address, the label of an environment that has not told us its name.
fn host_label(http_base: &str) -> String {
    url::Url::parse(http_base)
        .ok()
        .and_then(|url| url.host_str().map(|host| url.port().map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"))))
        .unwrap_or_else(|| http_base.to_owned())
}

impl SavedEnvironments {
    /// The single connection earlier builds saved, as the active environment.
    pub fn from_legacy(credentials: Credentials) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let environment = SavedEnvironment {
            id: id.clone(),
            environment_id: None,
            label: host_label(&credentials.http_base),
            http_base: credentials.http_base,
            access_token: Some(credentials.access_token),
            server_version: None,
            local: false,
            ssh: None,
            relay: None,
        };
        SavedEnvironments { active: Some(id), environments: vec![environment] }
    }

    pub fn get(&self, id: &str) -> Option<&SavedEnvironment> {
        self.environments.iter().find(|environment| environment.id == id)
    }

    pub fn active(&self) -> Option<&SavedEnvironment> {
        self.get(self.active.as_deref()?)
    }

    /// The entry for a server: the same `environmentId`, else the same address.
    pub fn find(&self, environment_id: Option<&str>, http_base: &str) -> Option<&SavedEnvironment> {
        environment_id
            .and_then(|wanted| self.environments.iter().find(|environment| environment.environment_id.as_deref() == Some(wanted)))
            .or_else(|| self.environments.iter().find(|environment| environment.http_base == http_base))
    }

    /// Saves a pairing and makes it active. Pairing a saved server again refreshes its entry
    /// instead of adding a second one, reached the way it was just paired: over `ssh` when that
    /// is set, else directly. Returns the entry's id.
    pub fn upsert(&mut self, paired: &Paired, local: bool) -> String {
        self.upsert_route(paired, local, None)
    }

    /// `upsert` for a pairing made over SSH.
    pub fn upsert_ssh(&mut self, paired: &Paired, target: t3_core::ssh::SshTarget) -> String {
        self.upsert_route(paired, false, Some(target))
    }

    fn upsert_route(&mut self, paired: &Paired, local: bool, ssh: Option<t3_core::ssh::SshTarget>) -> String {
        let descriptor = paired.descriptor.as_ref();
        let environment_id = descriptor.map(|d| d.environment_id.clone()).filter(|id| !id.is_empty());
        let http_base = &paired.credentials.http_base;
        let existing = self.find(environment_id.as_deref(), http_base).map(|environment| environment.id.clone());
        let id = existing.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let previous = existing.and_then(|id| self.get(&id).cloned());
        let label = descriptor
            .map(|d| d.label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .or_else(|| previous.as_ref().map(|environment| environment.label.clone()))
            .unwrap_or_else(|| host_label(http_base));
        let environment = SavedEnvironment {
            id: id.clone(),
            environment_id: environment_id.or_else(|| previous.as_ref().and_then(|environment| environment.environment_id.clone())),
            label,
            http_base: http_base.clone(),
            access_token: Some(paired.credentials.access_token.clone()),
            server_version: descriptor
                .map(|d| d.server_version.clone())
                .filter(|version| !version.is_empty())
                .or_else(|| previous.as_ref().and_then(|environment| environment.server_version.clone())),
            local: ssh.is_none() && (local || previous.is_some_and(|environment| environment.local)),
            ssh,
            relay: None,        };
        match self.environments.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => *entry = environment,
            None => self.environments.push(environment),
        }
        self.active = Some(id.clone());
        id
    }

    pub fn upsert_relay(&mut self, environment: t3_core::relay::Environment) -> String {
        let existing = self.find(Some(&environment.environment_id), &environment.endpoint.http_base_url).map(|e| e.id.clone());
        let id = existing.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let saved = SavedEnvironment { id: id.clone(), environment_id: Some(environment.environment_id.clone()), label: environment.label.clone(), http_base: environment.endpoint.http_base_url.trim_end_matches('/').into(), access_token: None, server_version: None, local: false, ssh: None, relay: Some(environment) };
        match self.environments.iter_mut().find(|e| e.id == id) { Some(entry) => *entry = saved, None => self.environments.push(saved) }
        self.active = Some(id.clone());
        id
    }

    /// Points a saved environment at another address of the same server, as when T3 Code on this
    /// Mac comes back on a new port. Returns whether it moved.
    pub fn relocate(&mut self, id: &str, http_base: &str) -> bool {
        match self.environments.iter_mut().find(|environment| environment.id == id) {
            Some(environment) if environment.http_base != http_base => {
                environment.http_base = http_base.to_owned();
                true
            }
            _ => false,
        }
    }

    /// The edits that turn `before` into this list.
    pub fn changes_since(&self, before: &SavedEnvironments) -> Vec<EnvironmentChange> {
        let mut changes: Vec<EnvironmentChange> =
            before.environments.iter().filter(|old| self.get(&old.id).is_none()).map(|old| EnvironmentChange::Remove(old.id.clone())).collect();
        changes.extend(self.environments.iter().filter(|environment| before.get(&environment.id) != Some(*environment)).cloned().map(|environment| EnvironmentChange::Put(Box::new(environment))));
        if self.active != before.active {
            changes.push(EnvironmentChange::SetActive(self.active.clone()));
        }
        changes
    }

    /// Applies one edit. An entry is matched by its id, else by the server it names, so two
    /// instances that paired the same server keep one entry.
    pub fn apply(&mut self, change: EnvironmentChange) {
        match change {
            EnvironmentChange::Put(environment) => {
                let environment = *environment;
                let slot = self.environments.iter().position(|entry| entry.id == environment.id).or_else(|| {
                    let wanted = environment.environment_id.as_deref()?;
                    self.environments.iter().position(|entry| entry.environment_id.as_deref() == Some(wanted))
                });
                match slot {
                    Some(index) => {
                        let replaced = std::mem::replace(&mut self.environments[index], environment);
                        if self.active.as_deref() == Some(replaced.id.as_str()) {
                            self.active = Some(self.environments[index].id.clone());
                        }
                    }
                    None => self.environments.push(environment),
                }
            }
            EnvironmentChange::Remove(id) => {
                self.remove(&id);
            }
            EnvironmentChange::SetActive(active) => self.active = active,
        }
    }

    /// Forgets an environment and its credential. Returns whether it was saved.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.environments.len();
        self.environments.retain(|environment| environment.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
        self.environments.len() != before
    }

    /// Drops a credential the server rejected; the entry stays so it can be paired again.
    pub fn revoke(&mut self, id: &str) {
        if let Some(environment) = self.environments.iter_mut().find(|environment| environment.id == id) {
            environment.access_token = None;
        }
    }

    /// Takes the name, id, and version the connected server reports. Returns whether anything
    /// changed.
    pub fn update_identity(&mut self, id: &str, environment_id: &str, label: &str, server_version: &str) -> bool {
        let Some(environment) = self.environments.iter_mut().find(|environment| environment.id == id) else { return false };
        let before = environment.clone();
        if !environment_id.is_empty() {
            environment.environment_id = Some(environment_id.to_owned());
        }
        if !label.trim().is_empty() {
            environment.label = label.trim().to_owned();
        }
        if !server_version.is_empty() {
            environment.server_version = Some(server_version.to_owned());
        }
        *environment != before
    }
}

/// One edit to the saved environments. The writer applies edits to the file as it is when it
/// writes, so a second app instance's entries survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentChange {
    Put(Box<SavedEnvironment>),
    Remove(String),
    SetActive(Option<String>),
}

fn decode_environments(environments: Option<&str>, legacy: Option<&str>) -> SavedEnvironments {
    if let Some(text) = environments {
        return serde_json::from_str(text).unwrap_or_default();
    }
    legacy.and_then(|text| serde_json::from_str::<Credentials>(text).ok()).map(SavedEnvironments::from_legacy).unwrap_or_default()
}

/// Saved environments, migrating the single connection of earlier builds on first read.
pub fn load_environments() -> SavedEnvironments {
    let Some(base) = base() else { return SavedEnvironments::default() };
    let environments = std::fs::read_to_string(base.join(ENVIRONMENTS)).ok();
    let legacy = environments.is_none().then(|| std::fs::read_to_string(base.join(LEGACY_CONNECTION)).ok()).flatten();
    let saved = decode_environments(environments.as_deref(), legacy.as_deref());
    if legacy.is_some() {
        save_environment_changes(saved.changes_since(&SavedEnvironments::default()));
    }
    saved
}

/// Queues edits for the background writer, which re-reads the file and applies them in order.
pub fn save_environment_changes(changes: Vec<EnvironmentChange>) {
    if !changes.is_empty() {
        queue(ENVIRONMENTS_KEY, Save::Environments(changes));
    }
}

/// Writes the whole list now. For tools; the app queues edits with `save_environment_changes`.
pub fn save_environments(saved: &SavedEnvironments) -> std::io::Result<()> {
    write_environments(&base().ok_or_else(|| std::io::Error::other("no config directory"))?, saved)
}

/// Lays edits over the saved environments as they are now. A file that exists but cannot be
/// read is left alone.
fn write_environment_changes(base: &Path, changes: Vec<EnvironmentChange>) -> std::io::Result<()> {
    let current = match std::fs::read_to_string(base.join(ENVIRONMENTS)) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut saved = decode_environments(current.as_deref(), None);
    for change in changes {
        saved.apply(change);
    }
    write_environments(base, &saved)
}

fn write_environments(base: &Path, saved: &SavedEnvironments) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(saved).map_err(std::io::Error::other)?;
    write_private(base, ENVIRONMENTS, &text)?;
    // Earlier builds read the legacy file; keep a forgotten credential from lingering there.
    let legacy = base.join(LEGACY_CONNECTION);
    if let Some(credentials) = std::fs::read_to_string(&legacy).ok().and_then(|text| serde_json::from_str::<Credentials>(&text).ok())
        && saved.find(None, &credentials.http_base).and_then(|environment| environment.access_token.as_deref()) != Some(credentials.access_token.as_str())
    {
        let _ = std::fs::remove_file(legacy);
    }
    Ok(())
}

/// An owner-only file written to a temporary and renamed over the target.
fn write_private(base: &Path, name: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(base)?;
    let temporary = base.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&temporary).and_then(|mut file| file.write_all(text.as_bytes())).and_then(|()| std::fs::rename(&temporary, base.join(name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The thread that was open last, restored on launch the way the web restores its route.
pub fn load_last_thread() -> Option<String> {
    std::fs::read_to_string(base()?.join("last-thread")).ok().map(|id| id.trim().to_owned()).filter(|id| !id.is_empty())
}

pub fn save_last_thread(thread_id: &str) {
    if let Some(base) = base() {
        let _ = std::fs::create_dir_all(&base);
        let _ = std::fs::write(base.join("last-thread"), thread_id);
    }
}

const CLIENT_SETTINGS: &str = "settings";

/// Per-device settings the web keeps in local storage (`ClientSettingsSchema` keys).
pub fn load_client_settings() -> Option<String> {
    std::fs::read_to_string(base()?.join(format!("{CLIENT_SETTINGS}.json"))).ok()
}

/// Queues edited client settings keys. The writer lays them over the file as it is at write
/// time, so a second app instance's saves survive; it skips the write while the file exists but
/// cannot be read, and backs up a file that is not a settings object before replacing it.
pub fn patch_client_settings(patch: serde_json::Map<String, serde_json::Value>) {
    if !patch.is_empty() {
        queue(CLIENT_SETTINGS, Save::Patch(patch));
    }
}

/// UI state the web keeps in `localStorage` (panel tabs and widths, diff selections), one JSON
/// file per store. A missing file or one that is not JSON reads as `None`; callers decode the
/// value leniently so one bad key never costs the others.
pub fn load_ui_state(name: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(base()?.join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Queues a save. One writer thread owns every UI state file and the saved environments, off the
/// UI thread: saves land in order, coalesced to the newest value per file, so an older snapshot
/// can never overwrite a newer one.
pub fn save_ui_state<T: serde::Serialize>(name: &str, value: &T) {
    let Ok(text) = serde_json::to_string(value) else { return };
    queue(name, Save::Replace(text));
}

#[derive(Debug, Clone, PartialEq)]
enum Save {
    Replace(String),
    Patch(serde_json::Map<String, serde_json::Value>),
    Environments(Vec<EnvironmentChange>),
}

fn queue(name: &str, save: Save) {
    static WRITER: OnceLock<Option<mpsc::Sender<(String, Save)>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(String, Save)>();
        let spawned = std::thread::Builder::new().name("t3-ui-state".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let pending = coalesce(std::iter::once(first).chain(rx.try_iter()));
                if let Some(base) = base() {
                    for (name, save) in pending {
                        match save {
                            Save::Replace(text) => write_atomically(&base, &name, &text),
                            Save::Patch(patch) => write_patch(&base, &name, &patch),
                            Save::Environments(changes) => {
                                if let Err(error) = write_environment_changes(&base, changes) {
                                    tracing::warn!(%error, "could not save environments");
                                }
                            }
                        }
                    }
                }
            }
        });
        spawned.ok().map(|_| tx)
    });
    if let Some(writer) = writer {
        let _ = writer.send((name.to_owned(), save));
    }
}

/// Keeps one save per file, in the order files were first saved: the newest replacement, with
/// later patches merged into it or into each other.
fn coalesce(saves: impl Iterator<Item = (String, Save)>) -> Vec<(String, Save)> {
    let mut pending: Vec<(String, Save)> = Vec::new();
    for (name, save) in saves {
        let Some(entry) = pending.iter_mut().find(|(existing, _)| *existing == name) else {
            pending.push((name, save));
            continue;
        };
        entry.1 = match (std::mem::replace(&mut entry.1, Save::Replace(String::new())), save) {
            (Save::Patch(mut earlier), Save::Patch(later)) => {
                earlier.extend(later);
                Save::Patch(earlier)
            }
            (Save::Replace(text), Save::Patch(later)) => {
                Save::Replace(t3_core::client_settings::merge_patch(Some(&text), &later).text)
            }
            (Save::Environments(mut earlier), Save::Environments(later)) => {
                earlier.extend(later);
                Save::Environments(earlier)
            }
            (_, later) => later,
        };
    }
    pending
}

fn write_patch(base: &Path, name: &str, patch: &serde_json::Map<String, serde_json::Value>) {
    let path = base.join(format!("{name}.json"));
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(%error, file = %path.display(), "settings not saved: the file exists but cannot be read");
            return;
        }
    };
    let merged = t3_core::client_settings::merge_patch(current.as_deref(), patch);
    if merged.backup {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        let backup = base.join(format!("{name}.json.corrupt-{stamp}"));
        if let Err(error) = std::fs::copy(&path, &backup) {
            tracing::warn!(%error, file = %path.display(), "settings not saved: could not back up the unreadable file");
            return;
        }
        tracing::warn!(backup = %backup.display(), "settings file was not valid JSON; kept a copy and started fresh");
    }
    write_atomically(base, name, &merged.text);
}

/// A uniquely named temporary file renamed over the target: a crash or a second app instance
/// never leaves half a file or clobbers another writer's temporary.
fn write_atomically(base: &Path, name: &str, text: &str) {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let _ = std::fs::create_dir_all(base);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = base.join(format!(".{name}.json.{}.{sequence}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, base.join(format!("{name}.json"))));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

/// The active environment's credentials (the probes and examples connect with these).
pub fn load() -> Option<Credentials> {
    load_environments().active()?.credentials()
}

/// Saves a pairing as the active environment.
pub fn save(credentials: &Credentials) -> std::io::Result<()> {
    let mut saved = load_environments();
    saved.upsert(&Paired { credentials: credentials.clone(), descriptor: None }, false);
    save_environments(&saved)
}

// Cloud OAuth credentials use the same owner-only, atomic secret storage as pairings.
pub(crate) fn load_cloud_token(base: &Path) -> std::io::Result<Option<t3_core::connect::Token>> {
    match std::fs::read(base.join("cloud-oauth.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub(crate) fn save_cloud_token(base: &Path, token: &t3_core::connect::Token) -> std::io::Result<()> {
    write_private(base, "cloud-oauth.json", &serde_json::to_string(token).map_err(std::io::Error::other)?)
}
pub(crate) fn clear_cloud_token(base: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(base.join("cloud-oauth.json")) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), result => result }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(value: serde_json::Value) -> Save {
        match value {
            serde_json::Value::Object(map) => Save::Patch(map),
            _ => unreachable!(),
        }
    }

    #[test]
    fn saves_coalesce_to_the_newest_value_per_file() {
        let saves = [("a", "1"), ("b", "1"), ("a", "2"), ("a", "3")].map(|(n, t)| (n.to_owned(), Save::Replace(t.to_owned())));
        let pending = coalesce(saves.into_iter());
        assert_eq!(pending, vec![("a".to_owned(), Save::Replace("3".to_owned())), ("b".to_owned(), Save::Replace("1".to_owned()))]);

        let saves = [
            ("settings".to_owned(), patch(serde_json::json!({"a": 1, "b": 1}))),
            ("settings".to_owned(), patch(serde_json::json!({"b": 2}))),
        ];
        assert_eq!(coalesce(saves.into_iter()), vec![("settings".to_owned(), patch(serde_json::json!({"a": 1, "b": 2})))]);
    }

    #[test]
    fn patches_land_on_the_latest_file_and_keep_a_corrupt_one() {
        let dir = std::env::temp_dir().join(format!("t3-settings-patch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let read = |dir: &Path| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        let Save::Patch(wide) = patch(serde_json::json!({"chatWidth": "wide"})) else { unreachable!() };

        // Another instance wrote a key since this one loaded.
        std::fs::write(dir.join("settings.json"), r#"{"sendShortcut": "mod-enter"}"#).unwrap();
        write_patch(&dir, "settings", &wide);
        assert_eq!(read(&dir), serde_json::json!({"sendShortcut": "mod-enter", "chatWidth": "wide"}));

        std::fs::write(dir.join("settings.json"), "{not json").unwrap();
        write_patch(&dir, "settings", &wide);
        assert_eq!(read(&dir), serde_json::json!({"chatWidth": "wide"}));
        let backups: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .filter(|name| name.starts_with("settings.json.corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join(&backups[0])).unwrap(), "{not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn paired(http_base: &str, token: &str, environment_id: &str, label: &str) -> Paired {
        let descriptor = (!environment_id.is_empty()).then(|| t3_core::server_config::EnvironmentDescriptor {
            environment_id: environment_id.into(),
            label: label.into(),
            server_version: "0.1.7".into(),
            ..Default::default()
        });
        Paired { credentials: Credentials { http_base: http_base.into(), access_token: token.into(), relay: None }, descriptor }
    }

    #[test]
    fn the_single_legacy_connection_migrates_as_the_active_environment() {
        let legacy = r#"{"http_base":"http://127.0.0.1:3773","access_token":"old"}"#;
        let saved = decode_environments(None, Some(legacy));
        let active = saved.active().expect("migrated entry is active");
        assert_eq!((active.label.as_str(), active.http_base.as_str()), ("127.0.0.1:3773", "http://127.0.0.1:3773"));
        assert_eq!(active.credentials().map(|c| c.access_token), Some("old".to_owned()));
        // Once environments are saved, the legacy file is never read again.
        assert_eq!(decode_environments(Some(r#"{"environments":[]}"#), Some(legacy)), SavedEnvironments::default());
        assert_eq!(decode_environments(None, Some("not json")), SavedEnvironments::default());
        assert_eq!(decode_environments(Some("{}"), None), SavedEnvironments::default());
    }

    #[test]
    fn pairing_a_saved_server_again_refreshes_its_entry_and_activates_it() {
        let mut saved = decode_environments(None, Some(r#"{"http_base":"http://127.0.0.1:3773","access_token":"old"}"#));
        let migrated = saved.active.clone().expect("active");
        // The local server pairs again: same address, now with a descriptor.
        let id = saved.upsert(&paired("http://127.0.0.1:3773", "new", "env-mac", "MacBook Pro"), true);
        assert_eq!(id, migrated);
        let studio = saved.upsert(&paired("https://studio.ts.net", "s1", "env-studio", "Studio"), false);
        assert_eq!((saved.environments.len(), saved.active.as_deref()), (2, Some(studio.as_str())));
        // Same environment through another address: still one entry, and it stays local.
        saved.upsert(&paired("http://192.168.1.4:3773", "lan", "env-mac", ""), false);
        let mac = saved.get(&id).expect("kept");
        assert_eq!((mac.label.as_str(), mac.http_base.as_str(), mac.local), ("MacBook Pro", "http://192.168.1.4:3773", true));
        assert_eq!((mac.server_version.as_deref(), saved.environments.len()), (Some("0.1.7"), 2));
        assert_eq!(saved.active.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn revoked_and_removed_environments() {
        let mut saved = SavedEnvironments::default();
        let mac = saved.upsert(&paired("http://127.0.0.1:3773", "t", "env-mac", "MacBook Pro"), true);
        saved.revoke(&mac);
        assert_eq!(saved.get(&mac).and_then(SavedEnvironment::credentials), None);
        assert_eq!(saved.active.as_deref(), Some(mac.as_str()), "a rejected entry stays until paired again or removed");
        assert!(saved.update_identity(&mac, "env-mac", "Mac mini", "0.2.0"));
        assert!(!saved.update_identity(&mac, "env-mac", "Mac mini", "0.2.0"));
        assert_eq!(saved.get(&mac).map(|e| e.label.as_str()), Some("Mac mini"));
        assert!(saved.remove(&mac));
        assert_eq!((saved.active.clone(), saved.environments.len()), (None, 0));
        assert!(!saved.remove(&mac));
    }

    #[test]
    fn edits_land_on_the_file_as_another_instance_left_it() {
        let dir = std::env::temp_dir().join(format!("t3-environments-merge-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let read = |dir: &Path| decode_environments(std::fs::read_to_string(dir.join(ENVIRONMENTS)).ok().as_deref(), None);

        // Both instances start from the same file.
        let mut mine = SavedEnvironments::default();
        let mac = mine.upsert(&paired("http://127.0.0.1:3773", "t", "env-mac", "MacBook Pro"), true);
        write_environments(&dir, &mine).expect("writes");
        let mut theirs = mine.clone();
        // The other instance pairs a second server and writes first.
        let studio = theirs.upsert(&paired("https://studio.ts.net", "s", "env-studio", "Studio"), false);
        write_environments(&dir, &theirs).expect("writes");

        // This one, still holding the old list, disconnects and renames its entry.
        let before = mine.clone();
        mine.active = None;
        mine.update_identity(&mac, "env-mac", "Mac mini", "0.1.8");
        write_environment_changes(&dir, mine.changes_since(&before)).expect("writes");
        let saved = read(&dir);
        assert_eq!(saved.get(&studio).map(|e| e.label.as_str()), Some("Studio"), "the other instance's pairing survives");
        assert_eq!((saved.get(&mac).map(|e| e.label.as_str()), saved.active.as_deref()), (Some("Mac mini"), None));

        // Removing only removes what this instance removed.
        let before = mine.clone();
        mine.remove(&mac);
        write_environment_changes(&dir, mine.changes_since(&before)).expect("writes");
        assert_eq!(read(&dir).environments.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [studio.as_str()]);

        // Both instances paired the same server: one entry, and the active one follows it.
        let mut file = read(&dir);
        let mut other = SavedEnvironments::default();
        other.upsert(&paired("http://127.0.0.1:3773", "t2", "env-mac", "MacBook Pro"), true);
        let mut again = SavedEnvironments::default();
        let mine_id = again.upsert(&paired("http://127.0.0.1:3773", "t3", "env-mac", "MacBook Pro"), true);
        for change in other.changes_since(&SavedEnvironments::default()).into_iter().chain(again.changes_since(&SavedEnvironments::default())) {
            file.apply(change);
        }
        assert_eq!((file.environments.len(), file.active.as_deref()), (2, Some(mine_id.as_str())));
        assert_eq!(file.active().and_then(|e| e.access_token.as_deref()), Some("t3"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_paired_over_ssh_keeps_its_target_until_paired_another_way() {
        let target = t3_core::ssh::SshTarget { alias: "devbox".into(), hostname: "devbox.example.com".into(), username: Some("julius".into()), port: None };
        let mut saved = SavedEnvironments::default();
        let devbox = saved.upsert_ssh(&paired("http://127.0.0.1:52001", "t", "env-devbox", "devbox"), target.clone());
        let text = serde_json::to_string(&saved).unwrap_or_default();
        assert!(text.contains(r#""ssh":{"alias":"devbox","hostname":"devbox.example.com","username":"julius","port":null}"#), "{text}");
        assert_eq!(decode_environments(Some(&text), None), saved);
        // The next session's tunnel listens elsewhere: the same entry, still over SSH.
        let again = saved.upsert_ssh(&paired("http://127.0.0.1:52002", "t2", "env-devbox", "devbox"), target.clone());
        assert_eq!((again.as_str(), saved.environments.len()), (devbox.as_str(), 1));
        assert_eq!(saved.get(&devbox).and_then(|e| e.ssh.clone()), Some(target));
        // A pairing link for the same machine reaches it directly from now on.
        saved.upsert(&paired("https://devbox.ts.net", "t3", "env-devbox", "devbox"), false);
        assert_eq!(saved.get(&devbox).map(|e| (e.ssh.is_none(), e.http_base.as_str())), Some((true, "https://devbox.ts.net")));
        let relay = t3_core::relay::Environment { environment_id: "env-devbox".into(), label: "devbox".into(), endpoint: t3_core::relay::Endpoint { http_base_url: "https://devbox.connect.test".into(), ws_base_url: "wss://devbox.connect.test/ws".into(), provider_kind: "cloudflare_tunnel".into() } };
        assert_eq!(saved.upsert_relay(relay), devbox);
        let text = serde_json::to_string(&saved).unwrap();
        let restored = decode_environments(Some(&text), None);
        let entry = restored.get(&devbox).unwrap();
        assert_eq!(entry.address(), "T3 Connect");
        assert!(entry.ssh.is_none() && entry.access_token.is_none() && entry.relay.is_some());
        saved.upsert(&paired("https://devbox.ts.net", "direct", "env-devbox", "devbox"), false);
        assert!(saved.get(&devbox).unwrap().relay.is_none());
        // Entries saved before SSH existed read as direct.
        assert_eq!(decode_environments(Some(r#"{"environments":[{"id":"a","label":"A","httpBase":"http://h"}]}"#), None).environments[0].ssh, None);
    }

    #[test]
    fn a_saved_server_moves_to_its_new_address() {
        let mut saved = SavedEnvironments::default();
        let mac = saved.upsert(&paired("http://127.0.0.1:3773", "t", "env-mac", "MacBook Pro"), true);
        assert!(!saved.relocate(&mac, "http://127.0.0.1:3773"));
        assert!(saved.relocate(&mac, "http://127.0.0.1:3774"));
        assert_eq!(saved.get(&mac).and_then(SavedEnvironment::credentials).map(|c| c.http_base), Some("http://127.0.0.1:3774".to_owned()));
        assert!(!saved.relocate("missing", "http://127.0.0.1:1"));
    }

    #[test]
    fn saved_environments_are_owner_only_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("t3-environments-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut saved = SavedEnvironments::default();
        saved.upsert(&paired("http://127.0.0.1:3773", "t", "env-mac", "MacBook Pro"), true);
        write_private(&dir, ENVIRONMENTS, &serde_json::to_string(&saved).unwrap_or_default()).expect("writes");
        let text = std::fs::read_to_string(dir.join(ENVIRONMENTS)).unwrap_or_default();
        assert_eq!(decode_environments(Some(&text), None), saved);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(ENVIRONMENTS)).map(|m| m.permissions().mode() & 0o777).unwrap_or_default();
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_writes_replace_the_file_and_leave_no_temporaries() {
        let dir = std::env::temp_dir().join(format!("t3-ui-state-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_atomically(&dir, "panels", "{\"a\":1}");
        write_atomically(&dir, "panels", "{\"a\":2}");
        assert_eq!(std::fs::read_to_string(dir.join("panels.json")).ok().as_deref(), Some("{\"a\":2}"));
        let leftovers = std::fs::read_dir(&dir).map(|entries| entries.count()).unwrap_or(0);
        assert_eq!(leftovers, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
