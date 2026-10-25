//! BluGHub: the tray app. Battery levels for the G915 and G935 now; lighting
//! and keys to follow.
//!
//!   BluGHub            start (or, if already running, say so from the tray)
//!   BluGHub --hidden   the same; used by "Start with Windows"
//!   BluGHub --quit     ask the running instance to quit

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keep_days = blu_ghub::config::Config::load(&blu_ghub::app_paths::config_path())
        .map(|c| c.settings.log_retention_days)
        .unwrap_or(7);
    let _log = blu_ghub::platform::init_logging(keep_days);
    let result = blu_ghub::app::run(&args);
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}
