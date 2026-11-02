//! Saved connection: one environment for now. Lives in the OS config dir, owner-only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use crate::auth::Credentials;

fn base() -> Option<PathBuf> {
    std::env::var_os("T3_NATIVE_HOME").map(PathBuf::from).or_else(|| dirs::config_dir().map(|dir| dir.join("T3 Code Native")))
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

/// Per-device settings the web keeps in local storage (`ClientSettingsSchema` keys).
pub fn load_client_settings() -> Option<String> {
    std::fs::read_to_string(base()?.join("settings.json")).ok()
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
    static WRITER: OnceLock<Option<mpsc::Sender<(String, String)>>> = OnceLock::new();
    let Ok(text) = serde_json::to_string(value) else { return };
    let writer = WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let spawned = std::thread::Builder::new().name("t3-ui-state".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let pending = coalesce(std::iter::once(first).chain(rx.try_iter()));
                if let Some(base) = base() {
                    for (name, text) in pending {
                        write_atomically(&base, &name, &text);
                    }
                }
            }
        });
        spawned.ok().map(|_| tx)
    });
    if let Some(writer) = writer {
        let _ = writer.send((name.to_owned(), text));
    }
}

/// Keeps the newest text per file, in the order files were first saved.
fn coalesce(saves: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut pending: Vec<(String, String)> = Vec::new();
    for (name, text) in saves {
        match pending.iter_mut().find(|(existing, _)| *existing == name) {
            Some(entry) => entry.1 = text,
            None => pending.push((name, text)),
        }
    }
    pending
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

    #[test]
    fn saves_coalesce_to_the_newest_value_per_file() {
        let saves = [("a", "1"), ("b", "1"), ("a", "2"), ("a", "3")].map(|(n, t)| (n.to_owned(), t.to_owned()));
        let pending = coalesce(saves.into_iter());
        assert_eq!(pending, vec![("a".to_owned(), "3".to_owned()), ("b".to_owned(), "1".to_owned())]);
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
