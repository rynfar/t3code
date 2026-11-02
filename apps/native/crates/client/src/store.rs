//! Saved connection: one environment for now. Lives in the OS config dir, owner-only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use crate::auth::Credentials;

static HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Points every read and write at `home` for the rest of the process, ahead of
/// `T3_NATIVE_HOME`. Tools that must not touch the caller's saved state call it before anything
/// loads. Only the first call takes effect.
pub fn use_home(home: PathBuf) {
    let _ = HOME_OVERRIDE.set(home);
}

fn base() -> Option<PathBuf> {
    HOME_OVERRIDE
        .get()
        .cloned()
        .or_else(|| std::env::var_os("T3_NATIVE_HOME").map(PathBuf::from))
        .or_else(|| dirs::config_dir().map(|dir| dir.join("T3 Code Native")))
}

fn path() -> Option<PathBuf> {
    Some(base()?.join("connection.json"))
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

/// Queues a save. One writer thread owns every UI state file: saves land in order, coalesced to
/// the newest value per file, so an older snapshot can never overwrite a newer one.
pub fn save_ui_state<T: serde::Serialize>(name: &str, value: &T) {
    let Ok(text) = serde_json::to_string(value) else { return };
    queue(name, Save::Replace(text));
}

#[derive(Debug, Clone, PartialEq)]
enum Save {
    Replace(String),
    Patch(serde_json::Map<String, serde_json::Value>),
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
            (_, replace @ Save::Replace(_)) => replace,
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

pub fn load() -> Option<Credentials> {
    let text = std::fs::read_to_string(path()?).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(credentials: &Credentials) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(credentials).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn forget() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
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
