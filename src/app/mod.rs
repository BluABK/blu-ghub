//! The tray app: one hidden window, one tray icon, one worker per device
//! connection.
//!
//! The main thread sits in `GetMessageW` and wakes only for something real:
//! the tray icon being clicked, a device arriving or leaving, the PC
//! resuming, or a worker reporting new battery state. No timers tick while
//! nothing happens.
//!
//! Re-entrancy: several Win32 calls (the tray menu's modal loop, the shell,
//! cross-process sends) dispatch other messages back into the window
//! procedure while they run. The app state lives in a thread-local `RefCell`,
//! so none of those calls may run while it is borrowed; the ones that pump
//! are made outside the borrow, and [`with_app`] declines a nested borrow
//! rather than panicking.

pub mod details;
pub mod devices;
mod dimmer;
mod eq_presets;
mod eq_widget;
mod keyboard_editor;
pub mod keyboard_gkeys;
mod macro_editor;
mod settings;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0,
    CM_NOTIFY_FILTER_0_0, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
    CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
};
use windows::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT,
    WAIT_ABANDONED_0, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW,
    ENDSESSION_CLOSEAPP, FindWindowW, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId,
    HICON, KillTimer, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
    PBT_APMRESUMEAUTOMATIC, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SM_CXSMICON, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, SendMessageTimeoutW, SetForegroundWindow,
    SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_NULL, WM_POWERBROADCAST,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config::Config;
use crate::hid;
use crate::hidpp::{self, HidppLock, LockState};
use crate::platform;
use devices::{Device, Link, Reporter, Shared, State, Status, WM_APP_STATE, Worker};

const CLASS: PCWSTR = w!("BluGHub.Core");
const WM_TRAY: u32 = WM_APP + 1;
/// A HID device arrived or left (posted from the hotplug callback).
const WM_APP_DEVCHANGE: u32 = WM_APP + 3;
/// A second launch wants this one to show itself. Answered with 1 by a live
/// instance, so a launch during shutdown can tell the difference.
const WM_APP_SHOW: u32 = WM_APP + 4;
/// The HID++ lock another process held has been released to us.
const WM_APP_LOCKED: u32 = WM_APP + 5;
/// Open the settings window (posted to self, so it opens outside whatever
/// asked for it). `wparam` > 0: the `--settings-gate` measurement, that many
/// open/close cycles.
const WM_APP_SETTINGS: u32 = WM_APP + 6;
/// The settings window saved the config: read it again and apply it.
pub(super) const WM_APP_CONFIG: u32 = WM_APP + 7;
/// Re-apply everything to the devices from scratch (the settings window's
/// button, or the tray menu).
pub(super) const WM_APP_REAPPLY: u32 = WM_APP + 8;

const TRAY_ID: u32 = 1;
/// Hotplug sends a burst of arrivals (one per HID collection); act once, a
/// moment after the last.
const DEVCHANGE_TIMER: usize = 1;
const DEVCHANGE_DELAY_MS: u32 = 400;
/// Retrying a tray icon that the shell did not accept (Explorer still
/// starting at sign-in).
const TRAY_RETRY_TIMER: usize = 2;
const TRAY_RETRY_MS: u32 = 3000;
const TRAY_RETRIES: u8 = 20;
/// The `--settings-gate` measurement's next step, a while after each close:
/// the Direct3D driver frees a closed window's memory some seconds late.
const GATE_TIMER: usize = 3;
const GATE_SETTLE_MS: u32 = 10_000;

const IDM_AUTOSTART: usize = 10;
const IDM_LOGS: usize = 11;
const IDM_REFRESH: usize = 12;
const IDM_QUIT: usize = 13;
const IDM_SETTINGS: usize = 14;
const IDM_REAPPLY: usize = 15;

struct App {
    hwnd: HWND,
    icon: HICON,
    shared: Shared,
    config: Config,
    /// Why the config file could not be read, if it could not: the app
    /// then runs on defaults and the settings window will not save over it.
    config_error: Option<String>,
    workers: Vec<Worker>,
    tray_ok: bool,
    tray_retries: u8,
    /// Devices already warned about, cleared once charged again.
    warned_low: [bool; 2],
    shut_down: bool,
    hidpp_lock: Option<HidppLock>,
    /// Waits for a probe to release the HID++ lock; stopped on shutdown.
    lock_waiter: Option<(hid::StopEvent, JoinHandle<()>)>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Kept outside `APP` so every unrelated message can be checked against
    /// it without borrowing the app state.
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    /// The app was told to quit while the settings window was open: the
    /// message loop ends once that window has closed.
    static QUIT_AFTER_SETTINGS: Cell<bool> = const { Cell::new(false) };
    /// `--settings-gate`: cycles done, cycles wanted.
    static GATE: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// The lock handed over by the waiter thread, collected on WM_APP_LOCKED.
static HANDED_LOCK: Mutex<Option<HidppLock>> = Mutex::new(None);

/// Run `f` on the app state. If it is already borrowed further up the stack
/// (a message dispatched from inside a call that pumps), skip rather than
/// panic: a skipped redraw is harmless, an abort is not.
/// What the keyboard's worker is to do, from the settings.
fn keyboard_setup(c: &Config) -> devices::KeyboardSetup {
    devices::KeyboardSetup {
        gkeys: keyboard_gkeys::Setup { gkeys: c.keyboard.gkeys.clone(), macros: c.macros.clone() },
        layout: c.keyboard.lighting.shown(),
        power: c.keyboard.power,
    }
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut a) => a.as_mut().map(f),
        Err(_) => {
            debug!("app state busy; message skipped");
            None
        }
    })
}

/// The running instance's window, if any.
fn running_window() -> Option<HWND> {
    unsafe { FindWindowW(CLASS, PCWSTR::null()) }.ok().filter(|h| !h.is_invalid())
}

/// `BluGHub --quit`: ask the running instance to quit and wait until it has.
fn quit_running() {
    let Some(h) = running_window() else {
        info!("--quit: BluGHub is not running");
        return;
    };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok();
    unsafe {
        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    if let Some(p) = process {
        // Stopping the workers takes at most a second or two.
        unsafe {
            WaitForSingleObject(p, 10_000);
            let _ = CloseHandle(p);
        }
    }
}

/// Become the one running instance. `None` means another instance is
/// running and has been told to show itself.
fn single_instance() -> Result<Option<HANDLE>> {
    // Created owned by this (the main) thread: when the process ends, a
    // second launch waiting on it gets it.
    let mutex = unsafe { CreateMutexW(None, true, w!("Local\\BluGHub")) }
        .context("creating the single-instance mutex")?;
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Ok(Some(mutex));
    }
    // Another instance. If it answers, it is alive: let it show itself.
    if let Some(h) = running_window() {
        // This launch was started by the user and may take the foreground;
        // the running instance, which opens the window, may not unless
        // handed the right.
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
            let _ = AllowSetForegroundWindow(pid);
        }
        let mut answer = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(
                h,
                WM_APP_SHOW,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                5000,
                Some(&mut answer),
            )
        };
        if ok.0 != 0 && answer == 1 {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Ok(None);
        }
    }
    // It is starting or shutting down: wait for it to finish, then take over.
    let r = unsafe { WaitForSingleObject(mutex, 15_000) };
    if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED_0 {
        Ok(Some(mutex))
    } else {
        unsafe {
            let _ = CloseHandle(mutex);
        }
        Ok(None)
    }
}

pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--quit") {
        quit_running();
        return Ok(());
    }
    let Some(mutex) = single_instance()? else { return Ok(()) };

    let (config, config_error) = match Config::load(&crate::app_paths::config_path()) {
        Ok(c) => (c.sanitized(), None),
        Err(e) => {
            warn!("{e:#}; running on defaults");
            (Config::default(), Some(format!("{e:#}")))
        }
    };
    info!("BluGHub {} starting", crate::version());

    if !cfg!(debug_assertions) {
        if let Err(e) = platform::create_start_menu_shortcut() {
            warn!("Start Menu shortcut: {e:#}");
        }
        // Same reason as the shortcut: a Run entry still pointing at a moved
        // exe fails silently at sign-in.
        if platform::starts_with_windows()
            && let Err(e) = platform::set_start_with_windows(true)
        {
            warn!("Start with Windows: {e:#}");
        }
    }

    let hwnd = create_window()?;
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    let icon = unsafe {
        CreateIconFromResourceEx(
            &crate::icon_art::dib(size),
            true,
            0x0003_0000,
            size as i32,
            size as i32,
            LR_DEFAULTCOLOR,
        )
    }
    .context("creating the tray icon")?;
    TASKBAR_CREATED.with(|t| t.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }));

    // Talking HID++ is exclusive to one blu-ghub process at a time. If a
    // probe holds it, wait for that in the background.
    let (hidpp_lock, lock_waiter) = match hidpp::try_lock() {
        Ok(LockState::Acquired(l)) => (Some(l), None),
        Ok(LockState::Busy(busy)) => {
            info!("another blu-ghub process is using the devices; waiting for it to finish");
            let stop = hid::StopEvent::new()?;
            let stop_raw = stop.raw().0 as isize;
            let hwnd_raw = hwnd.0 as isize;
            let t =
                std::thread::Builder::new().name("hidpp-lock-wait".into()).spawn(move || {
                    if let Some(lock) = busy.wait(HANDLE(stop_raw as *mut _)) {
                        *HANDED_LOCK.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(hwnd_raw as *mut _)),
                                WM_APP_LOCKED,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                })?;
            (None, Some((stop, t)))
        }
        Err(e) => {
            warn!("{e:#}; devices will not be read");
            (None, None)
        }
    };

    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            icon,
            shared: Shared::default(),
            config,
            config_error,
            workers: Vec::new(),
            tray_ok: false,
            tray_retries: 0,
            warned_low: [false; 2],
            shut_down: false,
            hidpp_lock,
            lock_waiter,
        })
    });
    with_app(|a| a.tray_add());
    let hotplug = register_hotplug(hwnd);
    with_app(|a| a.reconcile_workers());
    if let Some(i) = args.iter().position(|a| a == "--settings-gate") {
        let cycles = args.get(i + 1).and_then(|n| n.parse().ok()).unwrap_or(20usize).max(1);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_SETTINGS, WPARAM(cycles), LPARAM(0));
        }
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe { DispatchMessageW(&msg) };
    }

    if let Some(h) = hotplug {
        unsafe {
            let _ = CM_Unregister_Notification(h);
        }
    }
    APP.with(|a| a.borrow_mut().take());
    unsafe {
        let _ = CloseHandle(mutex);
    }
    info!("BluGHub stopped");
    Ok(())
}

fn create_window() -> Result<HWND> {
    let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance.into(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        bail!("registering the window class");
    }
    // A real but hidden top-level window rather than a message-only one:
    // only top-level windows get the TaskbarCreated broadcast (Explorer
    // restarted, tray icon must be re-added) and power notifications.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("BluGHub"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .context("creating the window")?;
    Ok(hwnd)
}

/// Device arrival/removal for HID interfaces, delivered on a thread pool
/// thread and forwarded to the window.
fn register_hotplug(hwnd: HWND) -> Option<HCMNOTIFICATION> {
    let filter = CM_NOTIFY_FILTER {
        cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        u: CM_NOTIFY_FILTER_0 {
            DeviceInterface: CM_NOTIFY_FILTER_0_0 { ClassGuid: unsafe { HidD_GetHidGuid() } },
        },
        ..Default::default()
    };
    let mut handle = HCMNOTIFICATION::default();
    let cr = unsafe {
        CM_Register_Notification(
            &filter,
            Some(hwnd.0 as *const c_void),
            Some(on_hotplug),
            &mut handle,
        )
    };
    if cr != CR_SUCCESS {
        warn!("device notifications unavailable ({cr:?}); plug changes need a restart");
        return None;
    }
    Some(handle)
}

unsafe extern "system" fn on_hotplug(
    _h: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    let hwnd = HWND(context as *mut _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_DEVCHANGE, WPARAM(0), LPARAM(0));
    }
    0
}

/// Sign-extended x and y from a packed `WPARAM`/`LPARAM` (GET_X_LPARAM).
fn point_from(v: usize) -> POINT {
    POINT { x: (v & 0xFFFF) as u16 as i16 as i32, y: ((v >> 16) & 0xFFFF) as u16 as i16 as i32 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event is in LOWORD(lparam), the
            // icon's anchor point in wparam.
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_SELECT + 1 {
                // The menu's modal loop dispatches other messages back into
                // this procedure: run it on a snapshot, outside the borrow.
                let snapshot = with_app(|a| {
                    (!a.shut_down)
                        .then(|| a.shared.lock().unwrap_or_else(|e| e.into_inner()).clone())
                });
                if let Some(Some(status)) = snapshot {
                    match show_menu(hwnd, &status, point_from(wparam.0)) {
                        IDM_LOGS => open_logs_folder(),
                        IDM_SETTINGS => {
                            open_settings(None);
                        }
                        cmd => {
                            with_app(|a| a.on_command(cmd));
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_STATE => {
            with_app(|a| a.on_state());
            settings::refresh();
            LRESULT(0)
        }
        WM_APP_DEVCHANGE => {
            unsafe { SetTimer(Some(hwnd), DEVCHANGE_TIMER, DEVCHANGE_DELAY_MS, None) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == DEVCHANGE_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), DEVCHANGE_TIMER);
            }
            with_app(|a| a.reconcile_workers());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TRAY_RETRY_TIMER => {
            with_app(|a| a.tray_add());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == GATE_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), GATE_TIMER);
            }
            settings_gate_step(hwnd);
            LRESULT(0)
        }
        WM_APP_SHOW => {
            // A second launch: show the settings window. Posted, as this is
            // answering another process, which waits.
            let alive = with_app(|a| !a.shut_down).unwrap_or(false);
            if alive {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_APP_SETTINGS, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(alive as isize)
        }
        WM_APP_CONFIG => {
            with_app(|a| a.reload_config());
            LRESULT(0)
        }
        WM_APP_REAPPLY => {
            with_app(|a| a.reapply());
            LRESULT(0)
        }
        WM_APP_SETTINGS => {
            match wparam.0 {
                0 => {
                    open_settings(None);
                }
                cycles => {
                    GATE.with(|g| g.set((0, cycles)));
                    settings_gate_step(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_APP_LOCKED => {
            let lock = HANDED_LOCK.lock().unwrap_or_else(|e| e.into_inner()).take();
            with_app(|a| {
                if let Some(l) = lock {
                    info!("the devices are free again");
                    a.hidpp_lock = Some(l);
                    a.reconcile_workers();
                }
            });
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                debug!("resumed from sleep: refreshing devices");
                with_app(|a| {
                    a.reconcile_workers();
                    a.workers.iter().for_each(|w| w.refresh());
                });
            }
            LRESULT(1)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            // The session is ending, or Restart Manager (an installer)
            // wants this app gone: stop now and leave.
            if lparam.0 as u32 & ENDSESSION_CLOSEAPP != 0 {
                info!("asked to close by Restart Manager");
            }
            with_app(|a| a.shutdown());
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(|a| a.shutdown());
            if settings::is_open() {
                // This is running inside the settings window's own message
                // loop: close the window, and end ours once it has returned.
                QUIT_AFTER_SETTINGS.with(|q| q.set(true));
                settings::close();
            } else {
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        _ => {
            let taskbar = TASKBAR_CREATED.with(|t| t.get());
            if taskbar != 0 && msg == taskbar {
                // Explorer restarted: the tray is new and empty.
                with_app(|a| {
                    a.tray_ok = false;
                    a.tray_retries = 0;
                    a.tray_add();
                });
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// Open the settings window and return once it is closed. Outside the
/// app-state borrow: the window runs a message loop of its own, which
/// dispatches this window's messages meanwhile.
/// Returns whether a window ran (rather than an open one being raised).
fn open_settings(auto_close: Option<Duration>) -> bool {
    let editing = with_app(|a| {
        (!a.shut_down).then(|| settings::Editing {
            shared: a.shared.clone(),
            config: a.config.clone(),
            config_error: a.config_error.clone(),
            tray: a.hwnd.0 as isize,
        })
    });
    let Some(Some(editing)) = editing else {
        return false;
    };
    let ran = match settings::run(editing, auto_close) {
        Ok(ran) => ran,
        Err(e) => {
            warn!("{e:#}");
            true
        }
    };
    if !settings::is_open() && QUIT_AFTER_SETTINGS.with(|q| q.replace(false)) {
        unsafe { PostQuitMessage(0) };
    }
    ran
}

/// `--settings-gate [N]`: open the settings window for 3 s and close it, N
/// times (20 by default), logging the process's private memory before the
/// first and 10 s after each close, to show that a closed window gives its
/// memory back and that repeated opening does not add up.
fn settings_gate_step(hwnd: HWND) {
    let (done, total) = GATE.with(|g| g.get());
    if done == 0 {
        info!("settings gate: private memory {} before", private_memory());
    } else {
        info!("settings gate: private memory {} 10 s after close {done}", private_memory());
    }
    if done == total {
        info!("settings gate: {total} cycles done");
        return;
    }
    let ran = open_settings(Some(Duration::from_secs(3)));
    if with_app(|a| a.shut_down).unwrap_or(true) {
        return;
    }
    // A window was already open (from the menu): this cycle did not happen.
    if ran {
        GATE.with(|g| g.set((done + 1, total)));
    }
    unsafe { SetTimer(Some(hwnd), GATE_TIMER, GATE_SETTLE_MS, None) };
}

/// This process's private memory, as "12.3 MiB".
fn private_memory() -> String {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    };
    match ok {
        Ok(()) => format!("{:.1} MiB", c.PrivateUsage as f64 / (1024.0 * 1024.0)),
        Err(e) => format!("unknown ({e})"),
    }
}

fn open_logs_folder() {
    let dir = HSTRING::from(crate::app_paths::logs_dir().as_os_str());
    let r = unsafe {
        ShellExecuteW(None, w!("open"), &dir, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if r.0 as isize <= 32 {
        warn!("could not open the log folder (ShellExecute {})", r.0 as isize);
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// One line per device, for the tooltip and the menu.
fn describe(device: Device, status: &Status) -> String {
    let s = status.device(device);
    let name = device.label();
    let off = if device == Device::Headset { "off" } else { "asleep" };
    match (&s.state, &s.battery) {
        (State::Absent, _) => format!("{name}: not connected"),
        (State::Connected, Some(b)) => format!("{name}: {}", b.shown()),
        (State::Connected, None) => format!("{name}: connected"),
        (State::Sleeping, Some(b)) => format!("{name}: {off} (last {})", b.shown()),
        (State::Sleeping, None) => format!("{name}: {off}"),
    }
}

impl App {
    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..Default::default()
        }
    }

    fn tooltip(&self) -> String {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        format!("BluGHub\n{}\n{}", describe(Device::Keyboard, &s), describe(Device::Headset, &s))
    }

    /// Add the icon; if the shell does not take it (Explorer still starting
    /// at sign-in), retry on a timer for a minute.
    fn tray_add(&mut self) {
        if self.shut_down || self.tray_ok {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TRAY_RETRY_TIMER);
            }
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icon;
        copy_wide(&mut nid.szTip, &self.tooltip());
        unsafe {
            // A timed-out add may have gone through anyway, in which case a
            // second add fails but a modify works.
            let added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool()
                || Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool();
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // Without version 4 the menu never opens: the callback would
            // carry old-style mouse messages instead.
            self.tray_ok = added && Shell_NotifyIconW(NIM_SETVERSION, &nid).as_bool();
        }
        if self.tray_ok {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TRAY_RETRY_TIMER);
            }
        } else if self.tray_retries < TRAY_RETRIES {
            self.tray_retries += 1;
            debug!("tray icon not accepted yet (attempt {})", self.tray_retries);
            unsafe { SetTimer(Some(self.hwnd), TRAY_RETRY_TIMER, TRAY_RETRY_MS, None) };
        } else {
            warn!("the tray icon could not be added");
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TRAY_RETRY_TIMER);
            }
        }
    }

    fn update_tooltip(&self) {
        let mut nid = self.nid();
        nid.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_wide(&mut nid.szTip, &self.tooltip());
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// A notification from the tray icon (shown as a toast on Windows 10/11).
    fn balloon(&self, title: &str, text: &str, warning: bool) {
        if self.shut_down {
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn on_state(&mut self) {
        if self.shut_down {
            return;
        }
        self.update_tooltip();
        let threshold = self.config.settings.low_battery_percent;
        if threshold == 0 {
            return;
        }
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for (i, device) in [Device::Keyboard, Device::Headset].into_iter().enumerate() {
            let d = s.device(device);
            let Some(b) = &d.battery else { continue };
            if d.state != State::Connected {
                continue;
            }
            // Warn once on the way down; re-arm after charging or a clear
            // margin above the threshold, so a reading that wobbles around
            // the line does not warn again and again.
            if !self.warned_low[i] && !b.charging && b.percent <= threshold {
                self.warned_low[i] = true;
                let text = format!("{} battery is at {}%.", device.label(), b.percent);
                self.balloon("Battery low", &text, true);
                info!("{text}");
            } else if self.warned_low[i] && (b.charging || b.percent > threshold.saturating_add(10))
            {
                self.warned_low[i] = false;
            }
        }
    }

    /// Start workers for connections whose hardware is present and not yet
    /// served; drop the ones whose hardware has gone.
    fn reconcile_workers(&mut self) {
        if self.shut_down || self.hidpp_lock.is_none() {
            return;
        }
        let (done, live): (Vec<Worker>, Vec<Worker>) =
            std::mem::take(&mut self.workers).into_iter().partition(|w| w.is_finished());
        self.workers = live;
        for w in done {
            w.stop();
        }
        let all = match hid::logitech_collections() {
            Ok(a) => a,
            Err(e) => {
                warn!("enumerating HID devices: {e:#}");
                return;
            }
        };
        let poll = Duration::from_secs(self.config.settings.battery_check_minutes * 60);
        for link in Link::ALL {
            if self.workers.iter().any(|w| w.link == link) {
                continue;
            }
            let rep = Reporter { shared: self.shared.clone(), hwnd: self.hwnd.0 as isize };
            let headset = self.config.headset.clone();
            let keyboard = keyboard_setup(&self.config);
            if let Some(w) = devices::start(link, &all, rep, poll, headset, keyboard) {
                self.workers.push(w);
            }
        }
    }

    fn shutdown(&mut self) {
        // Runs on WM_ENDSESSION and again on WM_DESTROY; only once matters.
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        let started = Instant::now();
        // The icon goes first, so it disappears even if a worker is slow.
        let nid = self.nid();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TRAY_RETRY_TIMER);
            let _ = KillTimer(Some(self.hwnd), DEVCHANGE_TIMER);
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        for w in std::mem::take(&mut self.workers) {
            w.stop();
        }
        if let Some((stop, t)) = self.lock_waiter.take() {
            stop.set();
            let _ = t.join();
        }
        self.hidpp_lock = None;
        unsafe {
            let _ = DestroyIcon(self.icon);
        }
        debug!("shut down in {:?}", started.elapsed());
    }

    /// The settings window saved the file: read it back and apply what
    /// changed. Workers take their settings when they start, so the ones
    /// affected are restarted (the headset's hands its buttons back and
    /// takes them again with the new actions).
    fn reload_config(&mut self) {
        if self.shut_down {
            return;
        }
        let new = match Config::load(&crate::app_paths::config_path()) {
            Ok(c) => c.sanitized(),
            Err(e) => {
                warn!("{e:#}; keeping the settings in use");
                return;
            }
        };
        self.config_error = None;
        let old = std::mem::replace(&mut self.config, new);
        let poll = old.settings.battery_check_minutes != self.config.settings.battery_check_minutes;
        let buttons = old.headset != self.config.headset;
        let keyboard = keyboard_setup(&old) != keyboard_setup(&self.config);
        info!(
            "settings saved{}{}",
            if poll || buttons { "; restarting workers" } else { "" },
            if keyboard && !poll { "; keyboard settings applied in place" } else { "" }
        );
        let (stop, keep): (Vec<Worker>, Vec<Worker>) = std::mem::take(&mut self.workers)
            .into_iter()
            .partition(|w| poll || (buttons && w.link == Link::Headset));
        self.workers = keep;
        for w in stop {
            w.stop();
        }
        if keyboard {
            let setup = keyboard_setup(&self.config);
            for w in &self.workers {
                w.update_keyboard(setup.clone());
            }
        }
        self.reconcile_workers();
        // The low-battery level applies from the next reading on.
        self.warned_low = [false; 2];
    }

    /// Everything back to the devices and taken again, painted from
    /// nothing: for when a device shows something other than the
    /// settings say.
    fn reapply(&mut self) {
        if self.shut_down {
            return;
        }
        info!("re-applying the settings to the devices from scratch");
        for w in std::mem::take(&mut self.workers) {
            w.stop();
        }
        self.reconcile_workers();
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            IDM_REAPPLY => self.reapply(),
            IDM_REFRESH => {
                // Also picks up a device whose worker could not start.
                self.reconcile_workers();
                self.workers.iter().for_each(|w| w.refresh());
            }
            IDM_AUTOSTART => {
                let on = !platform::starts_with_windows();
                if let Err(e) = platform::set_start_with_windows(on) {
                    warn!("Start with Windows: {e:#}");
                }
            }
            IDM_QUIT => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            _ => {}
        }
    }
}

/// Show the tray menu at `at` and return the chosen command (0 for none).
/// Runs without any borrow of the app state; see the call site.
fn show_menu(hwnd: HWND, status: &Status, at: POINT) -> usize {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else { return 0 };
    unsafe {
        for (i, device) in [Device::Keyboard, Device::Headset].into_iter().enumerate() {
            let line = HSTRING::from(describe(device, status));
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 100 + i, &line);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_REFRESH, w!("Read batteries now"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_REAPPLY, w!("Re-apply to the devices"));
        let autostart = if platform::starts_with_windows() { MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, MF_STRING | autostart, IDM_AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_LOGS, w!("Open log folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT, w!("Quit"));

        // Without this the menu does not close when you click elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(
            menu,
            (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0,
            at.x,
            at.y,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_points_keep_their_sign_on_monitors_left_of_the_primary() {
        // x = -100, y = 300
        let packed = ((300u32 << 16) | (-100i16 as u16 as u32)) as usize;
        let p = point_from(packed);
        assert_eq!((p.x, p.y), (-100, 300));
    }

    #[test]
    fn a_sleeping_keyboard_shows_its_last_reading() {
        let mut s = Status::default();
        s.keyboard_receiver.state = State::Sleeping;
        s.keyboard_receiver.battery = crate::battery::from_g915(&[0x0F, 0xEE, 0x00]);
        assert_eq!(describe(Device::Keyboard, &s), "\u{2328} G915: asleep (last \u{1F50B} 91%)");
        assert_eq!(describe(Device::Headset, &s), "\u{1F3A7} G935: not connected");
    }
}
