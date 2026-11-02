//! Saved connection: one environment for now. Lives in the OS config dir, owner-only.

use std::path::PathBuf;

use crate::auth::Credentials;

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("T3_NATIVE_HOME").map(PathBuf::from).or_else(|| dirs::config_dir().map(|dir| dir.join("T3 Code Native")))?;
    Some(base.join("connection.json"))
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
