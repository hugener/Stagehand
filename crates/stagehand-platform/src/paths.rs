use std::path::PathBuf;

/// The per-user directory Stagehand keeps its settings in, or `None` when the system
/// does not say where roaming application data lives.
pub fn config_dir() -> Option<PathBuf> {
    let roaming = std::env::var_os("APPDATA").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(roaming).join("Stagehand"))
}

/// The per-user directory Stagehand writes its log files to, or `None` when the system
/// does not say where local application data lives. Logs stay on this machine, so they
/// are kept out of the roaming settings directory.
pub fn log_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(local).join("Stagehand").join("logs"))
}
