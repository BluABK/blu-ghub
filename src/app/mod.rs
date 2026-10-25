//! The tray app: one hidden window, one tray icon, one worker per device.
//!
//! The main thread sits in `GetMessageW` and wakes only for something real:
//! the tray icon being clicked, a device arriving or leaving, the PC
//! resuming, or a worker reporting new battery state. No timers tick while
//! nothing happens.

pub mod devices;

use std::cell::RefCell;
use std::ffi::c_void;
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
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, KillTimer, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SW_SHOWNORMAL, SetForegroundWindow,
    SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_NULL, WM_POWERBROADCAST,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config::Config;
use crate::hid;
use crate::hidpp;
use crate::platform;
use devices::{Kind, Link, Reporter, Shared, Status, WM_APP_STATE, Worker};

const CLASS: PCWSTR = w!("BluGHub.Core");
const WM_TRAY: u32 = WM_APP + 1;
/// A HID device arrived or left (posted from the hotplug callback).
const WM_APP_DEVCHANGE: u32 = WM_APP + 3;
/// A second launch wants this one to show itself.
const WM_APP_SHOW: u32 = WM_APP + 4;

const TRAY_ID: u32 = 1;
/// Hotplug sends a burst of arrivals (one per HID collection); act once, a
/// moment after the last.
const DEVCHANGE_TIMER: usize = 1;
const DEVCHANGE_DELAY_MS: u32 = 400;

const IDM_AUTOSTART: usize = 10;
const IDM_LOGS: usize = 11;
const IDM_REFRESH: usize = 12;
const IDM_QUIT: usize = 13;

struct App {
    hwnd: HWND,
    icon: HICON,
    taskbar_created: u32,
    shared: Shared,
    config: Config,
    workers: Vec<Worker>,
    /// Devices already warned about, cleared once charged again.
    warned_low: [bool; 2],
    _hidpp_lock: Option<hidpp::HidppLock>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

/// Ask an already running instance to do something; true if one answered.
fn signal_running(msg: u32) -> bool {
    match unsafe { FindWindowW(CLASS, PCWSTR::null()) } {
        Ok(h) if !h.is_invalid() => {
            unsafe { PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0)) }.is_ok()
        }
        _ => false,
    }
}

pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--quit") {
        if !signal_running(WM_CLOSE) {
            info!("--quit: BluGHub is not running");
        }
        return Ok(());
    }

    // One instance per user session. A second launch (Start Menu, double
    // click) tells the first one to show itself and leaves.
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\BluGHub")) }
        .context("creating the single-instance mutex")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        signal_running(WM_APP_SHOW);
        unsafe {
            let _ = CloseHandle(mutex);
        }
        return Ok(());
    }

    let config = match Config::load(&crate::app_paths::config_path()) {
        Ok(c) => c,
        Err(e) => {
            warn!("{e:#}; running on defaults");
            Config::default()
        }
    };
    info!("BluGHub {} starting", crate::version());

    if !cfg!(debug_assertions)
        && let Err(e) = platform::create_start_menu_shortcut()
    {
        warn!("Start Menu shortcut: {e:#}");
    }

    // Talking HID++ is exclusive to one blu-ghub process at a time (the
    // diagnostics' probe takes the same lock).
    let lock = match hidpp::acquire_lock() {
        Ok(l) => Some(l),
        Err(e) => {
            warn!("{e:#}; devices will not be read");
            None
        }
    };

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
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    let has_lock = lock.is_some();
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            icon,
            taskbar_created,
            shared: Shared::default(),
            config,
            workers: Vec::new(),
            warned_low: [false; 2],
            _hidpp_lock: lock,
        })
    });
    with_app(|a| a.tray_add());
    let hotplug = register_hotplug(hwnd);
    if has_lock {
        with_app(|a| a.reconcile_workers());
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event is in LOWORD(lparam).
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_SELECT + 1 {
                with_app(|a| a.show_menu());
            }
            LRESULT(0)
        }
        WM_APP_STATE => {
            with_app(|a| a.on_state());
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
        WM_APP_SHOW => {
            with_app(|a| {
                a.balloon("BluGHub is already running", "It lives here in the tray.", false)
            });
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                debug!("resumed from sleep: refreshing devices");
                with_app(|a| a.workers.iter().for_each(|w| w.refresh()));
            }
            LRESULT(1)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            // Windows is shutting down: stop cleanly now, there is no later.
            with_app(|a| a.shutdown());
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
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            if with_app(|a| a.taskbar_created == msg).unwrap_or(false) {
                // Explorer restarted: the tray is new and empty.
                with_app(|a| a.tray_add());
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// One line per device, for the tooltip and the menu.
fn describe(kind: Kind, status: &Status) -> String {
    let s = status.get(kind);
    match (&s.link, &s.battery) {
        (Link::Absent, _) => format!("{}: not connected", kind.name()),
        (Link::Connected, Some(b)) => format!("{}: {}", kind.name(), b.short()),
        (Link::Connected, None) => format!("{}: connected", kind.name()),
        (Link::Sleeping, Some(b)) => {
            let what = if kind == Kind::Headset { "off" } else { "asleep" };
            format!("{}: {what} (last {})", kind.name(), b.short())
        }
        (Link::Sleeping, None) => {
            let what = if kind == Kind::Headset { "off" } else { "asleep" };
            format!("{}: {what}", kind.name())
        }
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
        let s = self.shared.lock().unwrap().clone();
        format!("BluGHub\n{}\n{}", describe(Kind::Keyboard, &s), describe(Kind::Headset, &s))
    }

    fn tray_add(&mut self) {
        let mut nid = self.nid();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icon;
        copy_wide(&mut nid.szTip, &self.tooltip());
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
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
        self.update_tooltip();
        let threshold = self.config.settings.low_battery_percent;
        if threshold == 0 {
            return;
        }
        let s = self.shared.lock().unwrap().clone();
        for (i, kind) in [Kind::Keyboard, Kind::Headset].into_iter().enumerate() {
            let Some(b) = &s.get(kind).battery else { continue };
            if s.get(kind).link != Link::Connected {
                continue;
            }
            // Warn once on the way down; re-arm after charging or a clear
            // margin above the threshold, so a reading that wobbles around
            // the line does not warn again and again.
            if !self.warned_low[i] && !b.charging && b.percent <= threshold {
                self.warned_low[i] = true;
                let text = format!("{} battery is at {}%.", kind.name(), b.percent);
                self.balloon("Battery low", &text, true);
                info!("{text}");
            } else if self.warned_low[i] && (b.charging || b.percent > threshold + 10) {
                self.warned_low[i] = false;
            }
        }
    }

    /// Start workers for devices that are present and not yet served; drop
    /// the ones whose device has gone.
    fn reconcile_workers(&mut self) {
        if self._hidpp_lock.is_none() {
            return;
        }
        let finished: Vec<Worker> = {
            let (done, live): (Vec<Worker>, Vec<Worker>) =
                std::mem::take(&mut self.workers).into_iter().partition(|w| w.is_finished());
            self.workers = live;
            done
        };
        for w in finished {
            w.stop();
        }
        let all = match hid::logitech_collections() {
            Ok(a) => a,
            Err(e) => {
                warn!("enumerating HID devices: {e:#}");
                return;
            }
        };
        let poll = Duration::from_secs(self.config.settings.battery_check_minutes.max(5) * 60);
        for kind in [Kind::Keyboard, Kind::Headset] {
            if self.workers.iter().any(|w| w.kind == kind) {
                continue;
            }
            let rep = Reporter { shared: self.shared.clone(), hwnd: self.hwnd.0 as isize };
            if let Some(w) = devices::start(kind, &all, rep, poll) {
                self.workers.push(w);
            }
        }
    }

    fn shutdown(&mut self) {
        let started = Instant::now();
        for w in std::mem::take(&mut self.workers) {
            w.stop();
        }
        debug!("workers stopped in {:?}", started.elapsed());
        let nid = self.nid();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = DestroyIcon(self.icon);
        }
    }

    fn show_menu(&mut self) {
        let s = self.shared.lock().unwrap().clone();
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else { return };
        unsafe {
            for (i, kind) in [Kind::Keyboard, Kind::Headset].into_iter().enumerate() {
                let line = HSTRING::from(describe(kind, &s));
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 100 + i, &line);
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, IDM_REFRESH, w!("Read batteries now"));
            let autostart = if platform::starts_with_windows() { MF_CHECKED } else { MF_STRING };
            let _ =
                AppendMenuW(menu, MF_STRING | autostart, IDM_AUTOSTART, w!("Start with Windows"));
            let _ = AppendMenuW(menu, MF_STRING, IDM_LOGS, w!("Open log folder"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT, w!("Quit"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Without this the menu does not close when you click elsewhere.
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenuEx(
                menu,
                (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0,
                pt.x,
                pt.y,
                self.hwnd,
                None,
            )
            .0 as usize;
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);

            match cmd {
                IDM_REFRESH => self.workers.iter().for_each(|w| w.refresh()),
                IDM_AUTOSTART => {
                    let on = !platform::starts_with_windows();
                    if let Err(e) = platform::set_start_with_windows(on) {
                        warn!("Start with Windows: {e:#}");
                    }
                }
                IDM_LOGS => {
                    let dir = HSTRING::from(crate::app_paths::logs_dir().as_os_str());
                    ShellExecuteW(
                        None,
                        w!("open"),
                        &dir,
                        PCWSTR::null(),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    );
                }
                IDM_QUIT => {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                _ => {}
            }
        }
    }
}
