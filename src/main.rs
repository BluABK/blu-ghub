//! BluGHub: the tray app. Battery levels for the G915 and G935, the G935's
//! buttons, and a settings window; lighting and macros to follow.
//!
//!   BluGHub                    start (or, if already running, open its
//!                              settings window)
//!   BluGHub --hidden           the same; used by "Start with Windows"
//!   BluGHub --quit             ask the running instance to quit
//!   BluGHub --settings-gate N  open and close the settings window N times
//!                              (20 by default), logging private memory

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Release builds abort on panic and have no console, so a crash would leave
/// no trace. Write one line straight to today's log file (not through the
/// logging thread, which an abort would cut short), hand the headset's
/// buttons back if blu-ghub holds them, then do the default.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let thread = std::thread::current();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("(no message)");
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let now = chrono::Local::now();
        let line = format!(
            "{} PANIC in thread '{}' at {at}: {msg}\n",
            now.format("%Y-%m-%dT%H:%M:%S%.6f"),
            thread.name().unwrap_or("?")
        );
        let file =
            blu_ghub::app_paths::logs_dir().join(format!("BluGHub.log.{}", now.format("%Y-%m-%d")));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
            let _ = f.write_all(line.as_bytes());
        }
        blu_ghub::app::devices::emergency_hand_back();
        blu_ghub::app::keyboard_gkeys::emergency_hand_back();
        default(info);
    }));
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keep_days = blu_ghub::config::Config::load(&blu_ghub::app_paths::config_path())
        .map(|c| c.sanitized().settings.log_retention_days)
        .unwrap_or(7);
    let _log = blu_ghub::platform::init_logging(keep_days);
    install_panic_hook();
    let result = blu_ghub::app::run(&args);
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}
