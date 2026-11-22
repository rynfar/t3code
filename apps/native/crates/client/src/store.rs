//! What the app keeps on this device, in the OS config dir: saved environments (owner-only, they
//! hold credentials), the last open thread, client settings, and UI state.
//!
//! Saved environments mirror the web's saved connection catalog (`client-runtime/src/connection/
//! catalog.ts`, `profileStore.ts`) in what this client uses: each paired server with its label,
//! address, and credential. One is connected at a time; the list is ready for more. The single
//! `connection.json` of earlier builds is still read when no `environments.json` exists.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedEnvironment {
    /// This device's key for the entry; stable across re-pairing.
    pub id: String,
    /// The server's own `environmentId`, once known.
    #[serde(default)]
    pub environment_id: Option<String>,
    pub label: String,
    /// Explicitly disconnected; stays saved until the user connects it again.
    #[serde(default)]
    pub disabled: bool,
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
}

impl SavedEnvironment {
    /// How the environment is reached, as its row shows it: `SSH user@host` over SSH, else its
    /// address (`environmentTransportLabel`).
    pub fn address(&self) -> String {
        match &self.ssh {
            Some(target) => format!("SSH {}", t3_core::ssh::format_target(target)),
            None => self.http_base.clone(),
        }
    }

    pub fn credentials(&self) -> Option<Credentials> {
        let access_token = self.access_token.clone()?;
        Some(Credentials { http_base: self.http_base.clone(), access_token })
    }

    pub fn has_connection_route(&self) -> bool {
        self.access_token.is_some() || has_saved_relay_route(&self.id)
    }

    /// Whether this saved environment is reached through a T3 Connect managed relay tunnel.
    pub fn is_relay_managed(&self) -> bool {
        is_relay_managed_environment(&self.id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedEnvironments {
    /// The environment to connect at launch; `None` after an explicit disconnect.
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default, deserialize_with = "deserialize_environments")]
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
            disabled: false,
            local: false,
            ssh: None,
        };
        SavedEnvironments { active: Some(id), environments: vec![environment] }
    }

    pub fn get(&self, id: &str) -> Option<&SavedEnvironment> {
        self.environments.iter().find(|environment| environment.id == id)
    }

    pub fn active(&self) -> Option<&SavedEnvironment> {
        self.get(self.active.as_deref()?)
    }

    pub fn find_by_environment_id(&self, id: &str) -> Option<&SavedEnvironment> {
        self.environments.iter().find(|environment| environment.environment_id.as_deref() == Some(id))
    }

    /// The entry for a server: the same `environmentId`, else the same address.
    pub fn find(&self, environment_id: Option<&str>, http_base: &str) -> Option<&SavedEnvironment> {
        environment_id
            .and_then(|wanted| self.find_by_environment_id(wanted))
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

    /// Saves or refreshes a T3 Connect relay-managed environment and makes it active.
    pub fn upsert_relay(&mut self, environment_id: &str, label: &str, http_base: &str, access_token: &str) -> String {
        self.upsert_relay_in(base(), environment_id, label, http_base, access_token)
    }

    fn upsert_relay_in(
        &mut self,
        dir: Option<PathBuf>,
        environment_id: &str,
        label: &str,
        http_base: &str,
        access_token: &str,
    ) -> String {
        let env_id = (!environment_id.trim().is_empty()).then(|| environment_id.trim().to_owned());
        let existing = env_id.as_deref().and_then(|id| self.find_by_environment_id(id))
            .map(|environment| environment.id.clone());
        let id = existing.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let previous = existing.and_then(|id| self.get(&id).cloned());
        let resolved_label = (!label.trim().is_empty())
            .then(|| label.trim().to_owned())
            .or_else(|| previous.as_ref().map(|environment| environment.label.clone()))
            .unwrap_or_else(|| host_label(http_base));
        let environment = SavedEnvironment {
            id: id.clone(),
            environment_id: env_id.or_else(|| previous.as_ref().and_then(|environment| environment.environment_id.clone())),
            label: resolved_label,
            disabled: false,
            http_base: http_base.to_owned(),
            access_token: Some(access_token.to_owned()),
            server_version: previous.as_ref().and_then(|environment| environment.server_version.clone()),
            local: false,
            ssh: None,
        };
        if let Some(prev) = previous.as_ref()
            && prev.http_base != http_base
            && prev.access_token.as_deref() != Some(access_token)
        {
            crate::dpop::remove_direct_credential_matching(&prev.http_base, prev.access_token.as_deref());
        }
        match self.environments.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => *entry = environment,
            None => self.environments.push(environment),
        }
        set_relay_managed_environment_in(dir, &id, true);
        self.active = Some(id.clone());
        id
    }

    /// `EnvironmentRegistry.assertKnownAddress` for untargeted SSH onboarding
    /// (`packages/client-runtime/src/connection/onboarding.ts`, `pingdotgg/t3code#37818`):
    /// refuses to attach a new SSH route to an environment already saved under a different route.
    pub fn check_ssh_onboarding_conflict(
        &self,
        environment_id: Option<&str>,
        target: &t3_core::ssh::SshTarget,
    ) -> Result<(), String> {
        let Some(env_id) = environment_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let Some(saved) = self
            .environments
            .iter()
            .find(|environment| environment.environment_id.as_deref() == Some(env_id))
        else {
            return Ok(());
        };
        let incoming_key = t3_core::ssh::target_connection_key(target);
        let same_ssh = saved
            .ssh
            .as_ref()
            .is_some_and(|existing| t3_core::ssh::target_connection_key(existing) == incoming_key);
        if same_ssh {
            return Ok(());
        }
        Err(format!(
            "That address claims to be {}, which is already saved. Add it as a route from that environment's settings instead.",
            saved.label
        ))
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
            disabled: false,
            http_base: http_base.clone(),
            access_token: Some(paired.credentials.access_token.clone()),
            server_version: descriptor
                .map(|d| d.server_version.clone())
                .filter(|version| !version.is_empty())
                .or_else(|| previous.as_ref().and_then(|environment| environment.server_version.clone())),
            local: ssh.is_none() && (local || previous.as_ref().is_some_and(|environment| environment.local)),
            ssh,
        };
        if let Some(prev) = previous.as_ref()
            && prev.http_base != *http_base
            && prev.access_token.as_deref() != Some(paired.credentials.access_token.as_str())
        {
            crate::dpop::remove_direct_credential_matching(&prev.http_base, prev.access_token.as_deref());
        }
        match self.environments.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => *entry = environment,
            None => self.environments.push(environment),
        }
        set_relay_managed_environment(&id, false);
        self.active = Some(id.clone());
        id
    }

    /// Points a saved environment at another address of the same server, as when T3 Code on this
    /// Mac comes back on a new port. Returns whether it moved.
    pub fn relocate(&mut self, id: &str, http_base: &str) -> bool {
        match self.environments.iter_mut().find(|environment| environment.id == id) {
            Some(environment) if environment.http_base != http_base => {
                let previous_http_base = environment.http_base.clone();
                environment.http_base = http_base.to_owned();
                crate::dpop::relocate_direct_credential(
                    &previous_http_base,
                    http_base,
                    environment.access_token.as_deref(),
                );
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
        self.remove_in(base(), id)
    }

    fn remove_in(&mut self, dir: Option<PathBuf>, id: &str) -> bool {
        let before = self.environments.len();
        if let Some(removed) = self.environments.iter().find(|environment| environment.id == id) {
            crate::dpop::remove_direct_credential_matching(&removed.http_base, removed.access_token.as_deref());
        }
        self.environments.retain(|environment| environment.id != id);
        set_relay_managed_environment_in(dir, id, false);
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
        self.environments.len() != before
    }

    /// Drops a credential the server rejected; the entry stays so it can be paired again.
    pub fn revoke(&mut self, id: &str) {
        if let Some(environment) = self.environments.iter_mut().find(|environment| environment.id == id) {
            crate::dpop::remove_direct_credential_matching(&environment.http_base, environment.access_token.as_deref());
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

fn deserialize_environments<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<SavedEnvironment>, D::Error> {
    let entries = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(entries.into_iter().filter_map(|entry| serde_json::from_value(entry).ok()).collect())
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
        let _ = queue(ENVIRONMENTS_KEY, Save::Environments(changes));
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
    write_private_file(base, name, text, true)
}

fn write_private_file(base: &Path, name: &str, text: &str, replace: bool) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(base)?;
    let temporary = base.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&temporary).and_then(|mut file| file.write_all(text.as_bytes())).and_then(|()| {
        let destination = base.join(name);
        if replace {
            std::fs::rename(&temporary, destination)
        } else {
            match std::fs::hard_link(&temporary, destination) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                result => result,
            }
        }
    });
    if !replace || written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The thread that was open last, restored on launch the way the web restores its route.
pub fn load_last_thread() -> Option<String> {
    std::fs::read_to_string(base()?.join("last-thread")).ok().map(|id| id.trim().to_owned()).filter(|id| !id.is_empty())
}

pub fn save_last_thread(thread_id: &str) {
    let thread_id = thread_id.to_owned();
    if let Some(writer) = writer() {
        let _ = writer.send(|_| {
            WriteRequest::Update(Box::new(move |base| {
                if let Some(base) = base {
                    let _ = std::fs::create_dir_all(&base);
                    let _ = std::fs::write(base.join("last-thread"), &thread_id);
                }
            }))
        });
        return;
    }
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
        let _ = queue(CLIENT_SETTINGS, Save::Patch(patch));
    }
}

/// UI state the web keeps in `localStorage` (panel tabs and widths, diff selections), one JSON
/// file per store. A missing file or one that is not JSON reads as `None`; callers decode the
/// value leniently so one bad key never costs the others.
pub fn load_project_preferences() -> t3_core::sidebar::grouping::ProjectPreferences {
    load_ui_state("sidebar-projects").and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default()
}

pub fn save_project_preferences(preferences: &t3_core::sidebar::grouping::ProjectPreferences) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static + use<> {
    let text = serde_json::to_string(preferences).map_err(|error| error.to_string());
    let (reply, response) = tokio::sync::oneshot::channel();
    let queued = text.and_then(|text| writer().ok_or_else(|| "UI writer unavailable".to_owned())?.send(|_| {
        WriteRequest::Update(Box::new(move |base| {
            let result = base.ok_or_else(|| "Native home unavailable".to_owned()).and_then(|base| write_atomically_result(&base, "sidebar-projects", &text).map_err(|error| error.to_string()));
            let _ = reply.send(result);
        }))
    }).map(|_| ()));
    async move { queued?; response.await.map_err(|error| error.to_string())? }
}

pub fn load_ui_state(name: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(base()?.join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Queues a save. One writer thread owns every UI state file and the saved environments, off the
/// UI thread: saves land in order, coalesced to the newest value per file, so an older snapshot
/// can never overwrite a newer one.
pub fn save_ui_state<T: serde::Serialize>(name: &str, value: &T) {
    let Ok(text) = serde_json::to_string(value) else { return };
    let _ = queue(name, Save::Replace(text));
}

#[derive(Clone, Copy)]
pub enum StarPromptStorage {
    Count,
    Read,
    Dismiss,
}

/// Confirmed, ordered storage; dismissal has its own file so a stale count cannot undo it.
pub async fn github_star_prompt(action: StarPromptStorage) -> Result<(u64, bool), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    writer().ok_or("UI writer unavailable")?.send(|_| {
        WriteRequest::Update(Box::new(move |base| {
            let result = base
                .ok_or_else(|| "Native home unavailable".to_owned())
                .and_then(|base| update_star_prompt(&base, action).map_err(|e| e.to_string()));
            let _ = tx.send(result);
        }))
    })?;
    rx.await.map_err(|e| e.to_string())?
}
fn update_star_prompt(base: &Path, action: StarPromptStorage) -> std::io::Result<(u64, bool)> {
    fn read<T: serde::de::DeserializeOwned + Default>(
        base: &Path,
        key: &str,
    ) -> std::io::Result<T> {
        match std::fs::read_to_string(base.join(format!("{key}.json"))) {
            Ok(text) => serde_json::from_str(&text).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e),
        }
    }
    const COUNT: &str = "github-star-prompt-sent";
    const DISMISSED: &str = "github-star-prompt-dismissed";
    // Dismiss must still work if the unrelated count was corrupted.
    if matches!(action, StarPromptStorage::Dismiss) {
        write_atomically_result(base, DISMISSED, "true")?;
        return Ok((0, true));
    }
    let dismissed = read::<bool>(base, DISMISSED)?;
    let mut sent = read::<u64>(base, COUNT)?;
    if matches!(action, StarPromptStorage::Count) && !dismissed {
        sent = sent.saturating_add(1);
        write_atomically_result(base, COUNT, &sent.to_string())?;
    }
    Ok((sent, dismissed))
}

#[derive(Debug, Clone, PartialEq)]
enum Save {
    Replace(String),
    Patch(serde_json::Map<String, serde_json::Value>),
    Environments(Vec<EnvironmentChange>),
}

type SaveReply = mpsc::Sender<Result<(), String>>;
type SavedRequest = (String, Save, Option<SaveReply>);
enum WriteRequest {
    Save(SavedRequest),
    Update(Box<dyn FnOnce(Option<PathBuf>) + Send>),
}
impl From<SavedRequest> for WriteRequest {
    fn from(value: SavedRequest) -> Self {
        Self::Save(value)
    }
}
struct Writer(std::sync::Mutex<(u64, mpsc::Sender<WriteRequest>)>);
impl Writer {
    fn send(&self, request: impl FnOnce(u64) -> WriteRequest) -> Result<u64, String> {
        let mut writer = self.0.lock().map_err(|e| e.to_string())?;
        writer.0 += 1;
        let sequence = writer.0;
        writer
            .1
            .send(request(sequence))
            .map_err(|e| e.to_string())?;
        Ok(sequence)
    }
}
fn writer() -> Option<&'static Writer> {
    static WRITER: OnceLock<Option<Writer>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<WriteRequest>();
            let spawned = std::thread::Builder::new()
                .name("t3-ui-state".into())
                .spawn(move || {
                    run_writer(rx, base);
                });
            spawned.ok().map(|_| Writer(std::sync::Mutex::new((0, tx))))
        })
        .as_ref()
}
fn run_writer<T: Into<WriteRequest>>(
    rx: mpsc::Receiver<T>,
    resolve_base: impl Fn() -> Option<PathBuf>,
) {
    while let Ok(first) = rx.recv() {
        let mut pending = Vec::new();
        for request in std::iter::once(first).chain(rx.try_iter()).map(Into::into) {
            match request {
                WriteRequest::Update(update) => {
                    write_pending(resolve_base(), std::mem::take(&mut pending));
                    update(resolve_base());
                }
                WriteRequest::Save((name, save, reply)) => {
                    if let Some(reply) = reply {
                        write_pending(resolve_base(), std::mem::take(&mut pending));
                        let result = resolve_base()
                            .ok_or_else(|| "Native home is unavailable.".to_owned())
                            .and_then(|base| match save {
                                Save::Patch(patch) => write_patch_result(&base, &name, &patch)
                                    .map_err(|e| e.to_string()),
                                _ => unreachable!("only settings patches request confirmation"),
                            });
                        let _ = reply.send(result);
                    } else {
                        pending.push((name, save));
                    }
                }
            }
        }
        write_pending(resolve_base(), pending);
    }
}
fn write_pending(base: Option<PathBuf>, pending: Vec<(String, Save)>) {
    if let Some(base) = base {
        for (name, save) in coalesce(pending.into_iter()) {
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
fn queue(name: &str, save: Save) -> Option<u64> {
    writer()?
        .send(|_| (name.to_owned(), save, None).into())
        .ok()
}
/// Commits a theme-library/selection transaction in writer order. Call off the UI thread.
pub fn commit_client_settings(
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    writer()
        .ok_or("Settings writer is unavailable.")?
        .send(|_| (CLIENT_SETTINGS.into(), Save::Patch(patch), Some(tx)).into())?;
    rx.recv().map_err(|e| e.to_string())?
}

pub struct ClientCommit {
    pub sequence: u64,
    pub patch: serde_json::Map<String, serde_json::Value>,
}
type ClientUpdateReply<T> = mpsc::Receiver<(T, Result<ClientCommit, String>)>;
fn enqueue_client_update<T: Send + 'static>(
    fallback: t3_core::client_settings::ClientSettingsDoc,
    prepare: impl FnOnce(&t3_core::client_settings::ClientSettingsDoc) -> (serde_json::Map<String, serde_json::Value>, T) + Send + 'static,
) -> Result<(u64, ClientUpdateReply<T>), String> {
    let (tx, rx) = mpsc::channel();
    let sequence = writer().ok_or("Settings writer is unavailable.")?.send(move |sequence| client_update_request(fallback, prepare, sequence, tx))?;
    Ok((sequence, rx))
}
fn client_update_request<T: Send + 'static>(
    fallback: t3_core::client_settings::ClientSettingsDoc,
    prepare: impl FnOnce(&t3_core::client_settings::ClientSettingsDoc) -> (serde_json::Map<String, serde_json::Value>, T) + Send + 'static,
    sequence: u64,
    tx: mpsc::Sender<(T, Result<ClientCommit, String>)>,
) -> WriteRequest {
    WriteRequest::Update(Box::new(move |base| {
        let current = base.ok_or_else(|| "Native home is unavailable.".to_owned()).and_then(|base| {
            let text = match std::fs::read_to_string(base.join("settings.json")) {
                Ok(text) => Ok(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(error) => Err(error.to_string()),
            }?;
            Ok((base, text))
        });
        let doc = current.as_ref().map(|(_, text)| t3_core::client_settings::ClientSettingsDoc::from_json(text)).unwrap_or(fallback);
        let (patch, value) = prepare(&doc);
        let result = current.and_then(|(base, _)| {
            if !patch.is_empty() {
                write_patch_result(&base, CLIENT_SETTINGS, &patch).map_err(|e| e.to_string())?;
            }
            Ok(ClientCommit { sequence, patch })
        });
        if let Err(error) = &result {
            tracing::warn!(%error,"settings not saved");
        }
        let _ = tx.send((value, result));
    }))
}
/// Prepares and commits against the current file in writer order. Call off the UI thread.
pub fn commit_client_settings_with<T: Send + 'static>(
    fallback: t3_core::client_settings::ClientSettingsDoc,
    prepare: impl FnOnce(&t3_core::client_settings::ClientSettingsDoc) -> (serde_json::Map<String, serde_json::Value>, T) + Send + 'static,
) -> Result<(T, Result<ClientCommit, String>), String> {
    let (_, rx) = enqueue_client_update(fallback, prepare)?;
    rx.recv().map_err(|e| e.to_string())
}
fn client_setting_request(
    fallback: t3_core::client_settings::ClientSettingsDoc,
    change: t3_core::client_settings::ClientSettingChange,
    sequence: u64,
    reply: mpsc::Sender<((), Result<ClientCommit, String>)>,
) -> WriteRequest {
    client_update_request(
        fallback,
        move |current| {
            let mut current = current.clone();
            (current.apply_change(&change), ())
        },
        sequence,
        reply,
    )
}

/// Queues a single-key edit against the latest document in writer order.
pub fn patch_client_setting_change(
    fallback: t3_core::client_settings::ClientSettingsDoc,
    change: t3_core::client_settings::ClientSettingChange,
) -> Option<u64> {
    let (reply, _) = mpsc::channel();
    writer()?.send(move |sequence| client_setting_request(fallback, change, sequence, reply)).ok()
}

fn rebase_theme_patch(
    mut patch: serde_json::Map<String, serde_json::Value>,
    before: Option<&serde_json::Value>,
    current: &t3_core::client_settings::ClientSettingsDoc,
) -> serde_json::Map<String, serde_json::Value> {
    if let Some(after) = patch.get(t3_core::themes::LIBRARY_KEY) {
        let library = t3_core::themes::transaction::rebase_library(
            before,
            after,
            current.entries().get(t3_core::themes::LIBRARY_KEY),
        );
        patch.insert(t3_core::themes::LIBRARY_KEY.into(), library);
    }
    patch
}
/// Queues only the changed themes, preserving other edits that reach the writer first.
pub fn patch_client_settings_rebased(
    patch: serde_json::Map<String, serde_json::Value>,
    before: Option<serde_json::Value>,
) -> Option<u64> {
    if patch.contains_key(t3_core::themes::LIBRARY_KEY) {
        enqueue_client_update(Default::default(), move |doc| {
            (rebase_theme_patch(patch, before.as_ref(), doc), ())
        })
        .ok()
        .map(|(sequence, _)| sequence)
    } else {
        queue(CLIENT_SETTINGS, Save::Patch(patch))
    }
}
pub fn commit_client_settings_rebased(
    patch: serde_json::Map<String, serde_json::Value>,
    before: Option<serde_json::Value>,
) -> Result<ClientCommit, String> {
    let (_, result) = commit_client_settings_with(Default::default(), move |doc| {
        (rebase_theme_patch(patch, before.as_ref(), doc), ())
    })?;
    result
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
    if let Err(error) = write_patch_result(base, name, patch) { tracing::warn!(%error, "settings not saved"); }
}
fn write_patch_result(base: &Path, name: &str, patch: &serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
    let path = base.join(format!("{name}.json"));
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(%error, file = %path.display(), "settings not saved: the file exists but cannot be read");
            return Err(error);
        }
    };
    let merged = t3_core::client_settings::merge_patch(current.as_deref(), patch);
    if merged.backup {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        let backup = base.join(format!("{name}.json.corrupt-{stamp}"));
        if let Err(error) = std::fs::copy(&path, &backup) {
            tracing::warn!(%error, file = %path.display(), "settings not saved: could not back up the unreadable file");
            return Err(error);
        }
        tracing::warn!(backup = %backup.display(), "settings file was not valid JSON; kept a copy and started fresh");
    }
    write_atomically_result(base, name, &merged.text)
}

/// A uniquely named temporary file renamed over the target: a crash or a second app instance
/// never leaves half a file or clobbers another writer's temporary.
fn write_atomically(base: &Path, name: &str, text: &str) {
    if name == t3_core::composer::drafts::STORE_NAME
        && let Err(error) = back_up_legacy_drafts(base, text)
    {
        tracing::warn!(%error, "drafts not saved: could not back up the pre-migration file");
        return;
    }
    let _ = write_atomically_result(base, name, text);
}

fn back_up_legacy_drafts(base: &Path, next: &str) -> std::io::Result<()> {
    const BACKUP: &str = "composer-drafts.v6.bak.json";
    fn version(text: &str) -> Option<u64> {
        #[derive(Deserialize)]
        struct Header { version: u64 }
        serde_json::from_str::<Header>(text).ok().map(|header| header.version)
    }
    if base.join(BACKUP).try_exists()? || version(next) != Some(7) {
        return Ok(());
    }
    let previous = match std::fs::read_to_string(base.join("composer-drafts.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if matches!(version(&previous), Some(1 | 3..=6)) {
        write_private_file(base, BACKUP, &previous, false)?;
    }
    Ok(())
}

fn write_atomically_result(base: &Path, name: &str, text: &str) -> std::io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(base)?;
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = base.join(format!(".{name}.json.{}.{sequence}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, base.join(format!("{name}.json"))));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
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

// Clerk Native Frontend API sessions are persisted privately (0o600) in `cloud-clerk.json`.
pub(crate) fn load_clerk_session(base: &Path) -> std::io::Result<Option<t3_core::connect::ClerkSession>> {
    match std::fs::read(base.join("cloud-clerk.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub(crate) fn save_clerk_session(base: &Path, session: &t3_core::connect::ClerkSession) -> std::io::Result<()> {
    write_private(base, "cloud-clerk.json", &serde_json::to_string(session).map_err(std::io::Error::other)?)
}
pub(crate) fn clear_clerk_session(base: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(base.join("cloud-clerk.json")) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), result => result }
}

const RELAY_MANAGED_FILE: &str = "relay-managed.json";

type RelayManagedMap = std::collections::HashMap<Option<PathBuf>, std::collections::HashSet<String>>;

fn relay_managed_store() -> &'static std::sync::Mutex<RelayManagedMap> {
    static STORE: OnceLock<std::sync::Mutex<RelayManagedMap>> = OnceLock::new();
    STORE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn load_relay_managed_file(base: &Path) -> std::collections::HashSet<String> {
    match std::fs::read_to_string(base.join(RELAY_MANAGED_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// Returns whether `id` is a T3 Connect relay-managed saved environment.
pub fn is_relay_managed_environment(id: &str) -> bool {
    is_relay_managed_environment_in(base(), id)
}

fn is_relay_managed_environment_in(base: Option<PathBuf>, id: &str) -> bool {
    let mut guard = relay_managed_store().lock().unwrap_or_else(|p| p.into_inner());
    let set = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_relay_managed_file).unwrap_or_default());
    set.contains(id)
}

/// Persists whether saved environment `id` is managed via T3 Connect relay.
pub fn set_relay_managed_environment(id: &str, relay_managed: bool) {
    set_relay_managed_environment_in(base(), id, relay_managed);
}

fn set_relay_managed_environment_in(base: Option<PathBuf>, id: &str, relay_managed: bool) {
    let mut guard = relay_managed_store().lock().unwrap_or_else(|p| p.into_inner());
    let set = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_relay_managed_file).unwrap_or_default());
    let changed = if relay_managed {
        set.insert(id.to_owned())
    } else {
        set.remove(id)
    };
    if changed
        && let Some(dir) = base.as_deref()
        && let Ok(text) = serde_json::to_string_pretty(set)
    {
        let _ = write_private(dir, RELAY_MANAGED_FILE, &text);
    }
}

type AlternateRoutes = std::collections::HashMap<String, Vec<crate::routes::CandidateRoute>>;
type AlternateRoutesCache = std::collections::HashMap<Option<PathBuf>, Arc<AlternateRoutes>>;

/// Settings renders read this on animation frames, and saves are queued; both need memory, not disk.
fn alternate_routes_cache() -> &'static std::sync::Mutex<AlternateRoutesCache> {
    static CACHE: OnceLock<std::sync::Mutex<AlternateRoutesCache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Saved alternate/learned routes keyed by saved environment ID (`CN-SESSION`).
pub fn load_alternate_routes() -> AlternateRoutes {
    (*alternate_routes_snapshot()).clone()
}

pub fn alternate_routes_snapshot() -> Arc<AlternateRoutes> {
    with_alternate_routes(Arc::clone)
}

fn with_alternate_routes<T>(read: impl FnOnce(&Arc<AlternateRoutes>) -> T) -> T {
    let mut cache = alternate_routes_cache().lock().unwrap_or_else(|p| p.into_inner());
    let routes = cache
        .entry(base())
        .or_insert_with(|| {
            Arc::new(load_ui_state("environment-routes")
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default())
        });
    read(routes)
}

pub fn has_saved_relay_route(id: &str) -> bool {
    with_alternate_routes(|saved| saved.get(id).is_some_and(|routes| {
        routes.iter().any(|route| route.kind() == crate::routes::ConnectionRouteKind::Relay)
    }))
}

/// Persists alternate/learned routes keyed by saved environment ID.
pub fn save_alternate_routes(routes: &AlternateRoutes) {
    let home = base();
    let Ok(text) = serde_json::to_string(routes) else { return };
    let mut cache = alternate_routes_cache().lock().unwrap_or_else(|p| p.into_inner());
    cache.insert(home.clone(), Arc::new(routes.clone()));
    if let Some(dir) = home
        && let Some(writer) = writer()
    {
        let _ = writer.send(|_| WriteRequest::Update(Box::new(move |_| {
            if let Err(error) = write_private(&dir, "environment-routes.json", &text) {
                tracing::warn!(%error, "could not save environment routes");
            }
        })));
    }
}

/// Stable 32-hex launcher ID for this T3 Code Native installation so renamed SSH connections keep
/// managing the remote server this app started (`pingdotgg/t3code#36351`).
pub fn ssh_launcher_id() -> Option<String> {
    let dir = base()?;
    ssh_launcher_id_in(&dir)
}

fn ssh_launcher_id_in(dir: &Path) -> Option<String> {
    let path = dir.join("ssh-launcher-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if t3_core::ssh::is_valid_launcher_id(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    let generated = uuid::Uuid::new_v4().simple().to_string();
    let _ = write_private(dir, "ssh-launcher-id", &generated);
    Some(generated)
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
    fn star_prompt_dismissal_survives_counts_reload_corruption_and_write_failure() {
        let dir = std::env::temp_dir().join(format!("t3-star-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sent in 1..=6 {
            assert_eq!(
                update_star_prompt(&dir, StarPromptStorage::Count).unwrap(),
                (sent, false)
            );
        }
        assert_eq!(
            update_star_prompt(&dir, StarPromptStorage::Read).unwrap(),
            (6, false)
        );
        update_star_prompt(&dir, StarPromptStorage::Dismiss).unwrap();
        std::fs::write(dir.join("github-star-prompt-sent.json"), "1").unwrap();
        assert_eq!(
            update_star_prompt(&dir, StarPromptStorage::Count).unwrap(),
            (1, true)
        );
        std::fs::write(dir.join("github-star-prompt-sent.json"), "bad").unwrap();
        assert!(update_star_prompt(&dir, StarPromptStorage::Read).is_err());
        assert!(update_star_prompt(&dir, StarPromptStorage::Dismiss).is_ok());
        std::fs::remove_file(dir.join("github-star-prompt-dismissed.json")).unwrap();
        std::fs::create_dir(dir.join("github-star-prompt-dismissed.json")).unwrap();
        assert!(update_star_prompt(&dir, StarPromptStorage::Count).is_err());
        assert!(update_star_prompt(&dir, StarPromptStorage::Dismiss).is_err());
        std::fs::remove_dir_all(dir).unwrap();
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
        Paired { credentials: Credentials { http_base: http_base.into(), access_token: token.into() }, descriptor }
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
        let mixed = decode_environments(Some(r#"{"active":"unknown","environments":[{"id":"known","label":"Mac","httpBase":"http://localhost","accessToken":"paired"},{"id":"unknown","label":"Unsupported","httpBase":"https://unused","kind":"future"},{"id":"malformed"}]}"#), None);
        assert_eq!(mixed.environments.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), ["known"]);
        assert!(mixed.active().is_none());
        assert_eq!(mixed.get("known").and_then(SavedEnvironment::credentials).map(|credentials| credentials.access_token), Some("paired".to_owned()));
    }

    #[test]
    fn relay_only_environment_has_a_connection_route_without_a_direct_credential() {
        use crate::routes::CandidateRoute;
        use t3_core::relay::{Endpoint, EnvironmentConnectResponse};

        let id = "relay-only-startup";
        let saved = decode_environments(Some(r#"{
            "active": "relay-only-startup",
            "environments": [{"id": "relay-only-startup", "label": "Relay", "httpBase": "https://host.example"}]
        }"#), None);
        let active = saved.active().unwrap();
        assert!(active.credentials().is_none());
        assert!(!active.has_connection_route());

        let route = CandidateRoute::relay(EnvironmentConnectResponse {
            environment_id: "relay-host".into(),
            endpoint: Endpoint {
                http_base_url: "https://host.example".into(),
                ws_base_url: "wss://host.example".into(),
                provider_kind: "cloudflare_tunnel".into(),
            },
            credential: "relay-ticket".into(),
            expires_at: "2026-11-21T00:00:00Z".into(),
        });
        let _ = load_alternate_routes();
        {
            let mut cache = alternate_routes_cache().lock().unwrap();
            Arc::make_mut(cache.entry(base()).or_default()).insert(id.into(), vec![route]);
        }
        let snapshot = alternate_routes_snapshot();
        assert!(Arc::ptr_eq(&snapshot, &alternate_routes_snapshot()));
        assert!(active.has_connection_route());
        Arc::make_mut(alternate_routes_cache().lock().unwrap().entry(base()).or_default()).remove(id);
        assert!(!active.has_connection_route());
        assert!(snapshot.contains_key(id));
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
        assert!(!saved.get(&mac).unwrap().disabled, "saved environments are enabled by default");
        saved.environments[0].disabled = true;
        let serialized = serde_json::to_string(&saved).unwrap();
        assert!(decode_environments(Some(&serialized), None).get(&mac).unwrap().disabled);
        saved.upsert(&paired("http://127.0.0.1:3773", "t", "env-mac", "MacBook Pro"), true);
        assert!(!saved.get(&mac).unwrap().disabled, "pairing again enables the environment");
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
    fn writer_barriers_commit_prior_patches_and_keep_later_writes_ordered() {
        let dir=std::env::temp_dir().join(format!("t3-theme-barrier-test-{}",std::process::id()));
        let _=std::fs::remove_dir_all(&dir);
        for (before,barrier,expected) in [
            (patch(serde_json::json!({"before":true,"value":1})),patch(serde_json::json!({"value":2})),serde_json::json!({"before":true,"value":2})),
            (Save::Replace("{\"after\":true,\"value\":3}".into()),patch(serde_json::json!({"final":true})),serde_json::json!({"after":true,"value":3,"final":true})),
        ] {
            let (tx,rx)=mpsc::channel();let (done,receipt)=mpsc::channel();
            tx.send((CLIENT_SETTINGS.into(),before,None)).unwrap();
            tx.send((CLIENT_SETTINGS.into(),barrier,Some(done))).unwrap();
            drop(tx);
            let home=dir.clone();let worker=std::thread::spawn(move||run_writer(rx,||Some(home.clone())));
            receipt.recv().unwrap().unwrap();worker.join().unwrap();
            let value:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
            assert_eq!(value,expected);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn grouping_override_edits_use_the_latest_record_and_preserve_other_instances() {
        use t3_core::client_settings::{ClientSettingChange, ClientSettingsDoc, ProjectGroupingMode};
        let dir = std::env::temp_dir().join(format!("t3-grouping-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cached = ClientSettingsDoc::from_json(r#"{"sidebarProjectGroupingOverrides":{"env:/a":"repository"}}"#);
        let latest = r#"{"futureSetting":42,"sidebarProjectGroupingOverrides":{"env:/a":"separate","other-env:/x":"separate"}}"#;
        for (mode, expected) in [
            (Some(ProjectGroupingMode::RepositoryPath), serde_json::json!({"env:/a":"repository_path","other-env:/x":"separate"})),
            (Some(ProjectGroupingMode::Repository), serde_json::json!({"env:/a":"repository","other-env:/x":"separate"})),
            (None, serde_json::json!({"other-env:/x":"separate"})),
        ] {
            std::fs::write(dir.join("settings.json"), latest).unwrap();
            let (tx, rx) = mpsc::channel();
            let (done, receipt) = mpsc::channel();
            tx.send(client_setting_request(cached.clone(), ClientSettingChange::ProjectGroupingOverride { key: "env:/a".into(), mode }, 1, done)).unwrap();
            drop(tx);
            let home = dir.clone();
            let worker = std::thread::spawn(move || run_writer(rx, || Some(home.clone())));
            receipt.recv().unwrap().1.unwrap();
            worker.join().unwrap();
            let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
            assert_eq!(saved["sidebarProjectGroupingOverrides"], expected);
            assert_eq!(saved["futureSetting"], 42);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn confirmed_theme_patch_reports_failure_and_keeps_the_prior_settings() {
        let dir=std::env::temp_dir().join(format!("t3-theme-commit-test-{}",std::process::id()));
        let _=std::fs::remove_dir_all(&dir);std::fs::create_dir_all(&dir).unwrap();
        let patch=serde_json::json!({"t3code:themes:v1":[{"id":"aurora"}],"t3code:theme":"aurora"}).as_object().unwrap().clone();
        write_atomically_result(&dir,"settings","{\"other\":42}").unwrap();
        write_patch_result(&dir,"settings",&patch).unwrap();
        let saved=std::fs::read_to_string(dir.join("settings.json")).unwrap();
        let value:serde_json::Value=serde_json::from_str(&saved).unwrap();assert_eq!(value["other"],42);assert_eq!(value["t3code:theme"],"aurora");
        std::fs::remove_file(dir.join("settings.json")).unwrap();std::fs::create_dir(dir.join("settings.json")).unwrap();
        assert!(write_patch_result(&dir,"settings",&patch).is_err());assert!(dir.join("settings.json").is_dir());
        std::fs::remove_dir_all(dir.join("settings.json")).unwrap();
        std::fs::write(dir.join("settings.json"),&saved).unwrap();
        std::fs::create_dir(dir.join("blocked.json")).unwrap();
        assert!(write_atomically_result(&dir,"blocked","replacement").is_err());
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(),saved);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn first_v7_draft_save_backs_up_the_pre_migration_file_once() {
        let dir = std::env::temp_dir().join(format!("t3-drafts-migration-{}", uuid::Uuid::new_v4()));
        let name = t3_core::composer::drafts::STORE_NAME;
        let original = "{\n  \"version\": 6, \"drafts\": {\"draft:p\": {\"prompt\": \"keep me\"}}\n}";
        write_atomically_result(&dir, name, original).unwrap();
        let save = |text: &str| {
            write_pending(Some(dir.clone()), vec![(name.into(), Save::Replace(text.into()))]);
        };
        let migrated = t3_core::composer::drafts::DraftStore::from_value(
            serde_json::from_str(original).ok().as_ref(),
        ).to_value().to_string();
        save(&migrated);
        let backup = dir.join("composer-drafts.v6.bak.json");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_to_string(dir.join("composer-drafts.json")).unwrap(), migrated);
        write_atomically_result(&dir, name, r#"{"version":6,"drafts":{}}"#).unwrap();
        save(&migrated);
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);
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

    #[test]
    fn ssh_launcher_id_is_stable_32_hex_across_reads() {
        let dir = std::env::temp_dir().join(format!("t3-launcher-id-test-{}", uuid::Uuid::new_v4()));
        let first = ssh_launcher_id_in(&dir).expect("generates launcher id");
        assert!(t3_core::ssh::is_valid_launcher_id(&first), "{first}");
        let second = ssh_launcher_id_in(&dir).expect("reuses launcher id");
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ssh_onboarding_refuses_to_attach_new_ssh_route_to_saved_environment() {
        let mut saved = SavedEnvironments::default();
        let paired_direct = paired("https://box.example.ts.net:3773", "tok-1", "env-1", "Remote devbox");
        saved.upsert(&paired_direct, false);

        let ssh_target = t3_core::ssh::SshTarget {
            alias: "devbox".into(),
            hostname: "devbox.example.test".into(),
            username: Some("developer".into()),
            port: Some(22),
        };
        let err = saved
            .check_ssh_onboarding_conflict(Some("env-1"), &ssh_target)
            .expect_err("untargeted SSH onboarding must not overwrite an existing direct environment");
        assert!(err.contains("Remote devbox"), "{err}");
        assert!(err.contains("already saved"), "{err}");

        // A distinct environment ID is allowed.
        assert!(saved.check_ssh_onboarding_conflict(Some("env-new"), &ssh_target).is_ok());

        // Re-adding the exact same saved SSH route is allowed.
        let paired_ssh = paired("http://127.0.0.1:41200", "tok-2", "env-ssh", "SSH Box");
        saved.upsert_ssh(&paired_ssh, ssh_target.clone());
        assert!(saved.check_ssh_onboarding_conflict(Some("env-ssh"), &ssh_target).is_ok());
    }

    #[test]
    fn clerk_session_storage_is_owner_only_and_relay_upsert_tracks_relay_managed_state() {
        let dir = std::env::temp_dir().join(format!("t3-clerk-store-test-{}", uuid::Uuid::new_v4()));
        let session = t3_core::connect::ClerkSession {
            client_token: "clerk-client-jwt".into(),
            session_id: "sess_123".into(),
            identity: Some("user@example.test".into()),
        };
        save_clerk_session(&dir, &session).unwrap();
        assert_eq!(load_clerk_session(&dir).unwrap(), Some(session));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("cloud-clerk.json")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        clear_clerk_session(&dir).unwrap();
        assert_eq!(load_clerk_session(&dir).unwrap(), None);

        let mut saved = SavedEnvironments::default();
        let id = saved.upsert_relay_in(
            Some(dir.clone()),
            "env-relay-1",
            "Studio",
            "https://studio.t3.codes",
            "relay-tok",
        );
        assert!(is_relay_managed_environment_in(Some(dir.clone()), &id));
        assert!(dir.join(RELAY_MANAGED_FILE).exists());
        assert_eq!(saved.get(&id).unwrap().environment_id.as_deref(), Some("env-relay-1"));
        assert!(saved.remove_in(Some(dir.clone()), &id));
        assert!(!is_relay_managed_environment_in(Some(dir.clone()), &id));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
