//! Saved connection: one environment for now. Lives in the OS config dir, owner-only.

use std::path::PathBuf;

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
/// file per store. A missing or unreadable file reads as `None`.
pub fn load_ui_state<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let text = std::fs::read_to_string(base()?.join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Written to a temporary file and renamed, so a crash never leaves half a file.
pub fn save_ui_state<T: serde::Serialize>(name: &str, value: &T) {
    let Some(base) = base() else { return };
    let Ok(text) = serde_json::to_string(value) else { return };
    let _ = std::fs::create_dir_all(&base);
    let temporary = base.join(format!(".{name}.json.tmp"));
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, base.join(format!("{name}.json")));
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
