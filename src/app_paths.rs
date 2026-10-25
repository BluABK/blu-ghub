//! Where blu-ghub keeps its things, all under `%APPDATA%\BluGHub`.

use std::path::{Path, PathBuf};

/// `%APPDATA%\BluGHub`, falling back to a directory beside the working
/// directory if the OS won't tell us.
pub fn data_dir() -> PathBuf {
    // Not ProjectDirs::data_dir() on Windows: it appends a further "data"
    // component, burying everything one level deeper than anyone would look.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("BluGHub");
    }
    directories::ProjectDirs::from("", "", "BluGHub")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("BluGHub-data"))
}

/// Output of the diagnostic tools: key-timing logs, sniffer captures,
/// descriptor dumps. Kept apart from the app's own logs because these are
/// evidence files the user may want to keep or send, not rolling noise.
pub fn diag_dir() -> PathBuf {
    data_dir().join("diag")
}

/// The app's own daily logs.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Environment override for the config file, so tests and throwaway runs can
/// never touch the real one.
pub const CONFIG_ENV: &str = "BLUGHUB_CONFIG";

pub fn config_path() -> PathBuf {
    match std::env::var_os(CONFIG_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => data_dir().join("BluGHub.toml"),
    }
}

/// Create `dir` if missing, ignoring the error: the write that follows will
/// report the real problem, with the file's path attached.
pub fn ensure_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
}
