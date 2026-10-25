//! Windows integration that is not about devices: the Start Menu shortcut,
//! starting with Windows, and logging.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::app_paths;

pub const APP_NAME: &str = "BluGHub";

/// The Start Menu folder shared with the author's other tools, so they group
/// together instead of scattering through Programs.
pub const START_MENU_FOLDER: &str = "Blu Software";

fn start_menu_shortcut_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(START_MENU_FOLDER)
            .join(format!("{APP_NAME}.lnk")),
    )
}

/// Create or overwrite the Start Menu shortcut so it points at the running
/// exe. Rewritten on every launch: a shortcut left pointing at a moved or
/// renamed build fails silently from the Start Menu.
/// Whether G HUB's agent is running. It takes the headset's buttons and
/// answers its mute button itself, so blu-ghub keeps out of its way.
pub fn ghub_running() -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };
    let snapshot = crate::hid::OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        let name = &entry.szExeFile;
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        if String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("lghub_agent.exe") {
            return true;
        }
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    false
}

pub fn create_start_menu_shortcut() -> Result<PathBuf> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    let exe = std::env::current_exe().context("finding the running exe")?;
    let lnk = start_menu_shortcut_path().ok_or_else(|| anyhow!("APPDATA is not set"))?;
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    unsafe {
        // S_FALSE (already initialised) is success too; only balance a
        // successful init, or a caller that had COM up would lose it.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(parent) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(parent.as_os_str()))?;
            }
            link.SetDescription(&HSTRING::from(
                "Battery, lighting and keys for a G915 and a G935",
            ))?;
            // The exe's first icon resource, the one build.rs embeds.
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
            Ok(())
        })();
        if com.is_ok() {
            CoUninitialize();
        }
        result.context("writing the Start Menu shortcut")?;
    }
    Ok(lnk)
}

fn auto_launch() -> Result<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().context("finding the running exe")?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name(APP_NAME)
        .set_app_path(&exe.display().to_string())
        .set_args(&["--hidden"])
        // Per user only: run elevated, the default would write the
        // all-users Run key, which the app then cannot remove unelevated.
        .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
        .build()
        .map_err(|e| anyhow!("{e}"))
}

/// Whether the HKCU Run entry exists.
pub fn starts_with_windows() -> bool {
    auto_launch().and_then(|a| a.is_enabled().map_err(|e| anyhow!("{e}"))).unwrap_or(false)
}

pub fn set_start_with_windows(on: bool) -> Result<()> {
    let a = auto_launch()?;
    if on { a.enable() } else { a.disable() }.map_err(|e| anyhow!("{e}"))
}

/// Logging to `%APPDATA%\BluGHub\logs\BluGHub.log.<date>`, one file a day,
/// old ones removed at start-up and at each daily rollover. If the folder
/// cannot be written, the app still runs (logging to nowhere). Keep the
/// returned guard alive: dropping it stops the writer.
pub fn init_logging(keep_days: u64) -> tracing_appender::non_blocking::WorkerGuard {
    let dir = app_paths::logs_dir();
    app_paths::ensure_dir(&dir);
    prune_old_logs(&dir, keep_days);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("BluGHub.log")
        .max_log_files(keep_days.max(1) as usize)
        .build(&dir);
    let (writer, guard) = match appender {
        Ok(a) => tracing_appender::non_blocking(a),
        Err(e) => {
            eprintln!("log file unavailable ({e}); continuing without one");
            tracing_appender::non_blocking(std::io::sink())
        }
    };
    // Most of the code lives in the library crate, whose log target is
    // `blu_ghub`; name it, or its debug lines never reach the file. The
    // settings window's graphics stack is chatty at info (every open lists
    // the GPUs), so it only gets to report problems.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,blu_ghub=debug,BluGHub=debug,wgpu_core=warn,wgpu_hal=warn,naga=warn,\
             egui_wgpu=warn,eframe=warn,egui_winit=warn,winit=warn",
        )
    });
    let file = fmt::layer().with_ansi(false).with_target(false).with_writer(writer);
    let stderr = fmt::layer().with_target(false).with_writer(std::io::stderr);
    tracing_subscriber::registry().with(filter).with(file).with(stderr).init();
    guard
}

/// Delete log files older than `keep_days`. A rolled file is
/// `BluGHub.log.2026-10-25`, whose "extension" is the date, hence the
/// `.log.` match.
fn prune_old_logs(dir: &std::path::Path, keep_days: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let Some(cutoff) = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(keep_days.max(1).saturating_mul(86400)))
    else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.contains(".log.") {
            continue;
        }
        let old = entry.metadata().and_then(|m| m.modified()).map(|m| m < cutoff).unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
