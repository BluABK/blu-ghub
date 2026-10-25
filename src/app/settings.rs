//! The settings window. Opened from the tray on demand and gone once closed:
//! while it is not in use there is no window, no renderer and no GPU memory
//! (the Direct3D driver hands its memory back some seconds after a close).
//!
//! [`run`] blocks the main thread until the window closes. eframe runs its
//! own message loop meanwhile, and that loop dispatches the tray window's
//! messages too, so the tray keeps working: its window procedure just runs
//! one level deeper. eframe keeps its event loop for the thread between runs,
//! which is what lets the window be opened again. Two things follow:
//!
//! - That loop swallows `WM_QUIT`, so quitting while the window is open has
//!   to close the window and quit afterwards (see the tray's `WM_DESTROY`).
//! - [`run`] must never be entered again while it runs: winit panics, and a
//!   panic in a window procedure aborts. [`is_open`] covers the whole call.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, GetSystemMetrics, HICON, ICON_BIG, ICON_SMALL, IsIconic,
    LR_DEFAULTCOLOR, PostMessageW, SM_CXICON, SM_CXSMICON, SW_RESTORE, SendMessageW,
    SetForegroundWindow, ShowWindow, WM_CLOSE, WM_SETICON,
};

use super::devices::{Device, Shared, State, Status};

thread_local! {
    /// From entering [`run`] until it returns, including eframe's start-up
    /// before the window exists.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// The open window, once it exists.
    static WINDOW: Cell<Option<isize>> = const { Cell::new(None) };
    /// Its context, so the tray's window procedure can ask for a redraw.
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

pub fn is_open() -> bool {
    RUNNING.with(|r| r.get())
}

fn window() -> Option<HWND> {
    WINDOW.with(|w| w.get()).map(|h| HWND(h as *mut _))
}

/// Redraw: the device state changed.
pub fn refresh() {
    CONTEXT.with(|c| {
        if let Some(ctx) = &*c.borrow() {
            ctx.request_repaint();
        }
    });
}

/// Bring the window to the front: it was asked for while already open.
/// Plain Win32 rather than egui's `Focus` command, which on Windows fakes an
/// Alt key press to be allowed to take the foreground.
pub fn raise() {
    if let Some(h) = window() {
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    }
}

/// Close the window, so [`run`] returns (the app is quitting).
pub fn close() {
    if let Some(h) = window() {
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Direct3D 12 only. eframe's default also offers Vulkan and OpenGL, and
/// wgpu loads every offered backend's driver just to list its adapters, so
/// the drivers this choice exists to avoid would be loaded anyway.
/// `WGPU_BACKEND` still overrides it.
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends =
        eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
    eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// winit registers for raw mouse and keyboard input when it creates its
/// event loop, which wakes this thread at every mouse movement anywhere on
/// screen while the window has the focus (and queues them up while it is
/// closed). egui only uses it for mouse-movement deltas, which this window
/// has no use for.
fn drop_raw_input() {
    let devices = [0x02u16, 0x06].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: usage,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    });
    unsafe {
        let _ = RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    }
}

fn icon(size_metric: windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX) -> HICON {
    let size = unsafe { GetSystemMetrics(size_metric) }.max(16) as u32;
    unsafe {
        CreateIconFromResourceEx(
            &crate::icon_art::dib(size),
            true,
            0x0003_0000,
            size as i32,
            size as i32,
            LR_DEFAULTCOLOR,
        )
    }
    .unwrap_or_default()
}

/// Open the window and return once it has closed. `auto_close` closes it by
/// itself after that long (for measuring open/close cycles).
///
/// Must not be called while the app state is borrowed: the tray's messages
/// are dispatched while it runs.
pub fn run(shared: Shared, auto_close: Option<Duration>) -> Result<()> {
    if is_open() {
        raise();
        return Ok(());
    }
    RUNNING.with(|r| r.set(true));
    // The window's icons, set by hand: eframe's own way picks whichever
    // window is active during the first frame (here, likely the tray's) and
    // never frees what it creates.
    let icons = [(ICON_SMALL, icon(SM_CXSMICON)), (ICON_BIG, icon(SM_CXICON))];
    let opts = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        viewport: egui::ViewportBuilder::default()
            .with_title("BluGHub")
            .with_inner_size([440.0, 220.0])
            .with_min_inner_size([360.0, 180.0])
            // An empty icon turns eframe's icon handling off; see above.
            .with_icon(egui::IconData::default())
            // Otherwise each open initialises OLE on this thread for good.
            .with_drag_and_drop(false),
        ..Default::default()
    };
    let result = eframe::run_native(
        "BluGHub",
        opts,
        Box::new(move |cc| {
            drop_raw_input();
            if let Ok(h) = cc.window_handle()
                && let RawWindowHandle::Win32(w) = h.as_raw()
            {
                let hwnd = HWND(w.hwnd.get() as *mut _);
                WINDOW.with(|c| c.set(Some(w.hwnd.get())));
                for (kind, icon) in icons {
                    unsafe {
                        SendMessageW(
                            hwnd,
                            WM_SETICON,
                            Some(WPARAM(kind as usize)),
                            Some(LPARAM(icon.0 as isize)),
                        );
                    }
                }
            }
            // No blinking text cursor: it would redraw twice a second for as
            // long as a text field has the focus.
            cc.egui_ctx.all_styles_mut(|s| s.visuals.text_cursor.blink = false);
            CONTEXT.with(|c| *c.borrow_mut() = Some(cc.egui_ctx.clone()));
            Ok(Box::new(Settings::new(shared, auto_close)))
        }),
    );
    CONTEXT.with(|c| *c.borrow_mut() = None);
    WINDOW.with(|w| w.set(None));
    RUNNING.with(|r| r.set(false));
    for (_, icon) in icons {
        if !icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
    result.map_err(|e| anyhow!("settings window: {e}"))
}

struct Settings {
    shared: Shared,
    opened: Instant,
    auto_close: Option<Duration>,
    /// Passes of `logic` and painted frames, logged on close: an idle window
    /// should have next to none. With when each frame was painted and what
    /// asked for it, for finding out why when it does not.
    passes: u32,
    painted: u32,
    frame_times: Vec<f32>,
    causes: std::collections::BTreeMap<String, u32>,
}

impl Settings {
    fn new(shared: Shared, auto_close: Option<Duration>) -> Self {
        Self {
            shared,
            opened: Instant::now(),
            auto_close,
            passes: 0,
            painted: 0,
            frame_times: Vec::new(),
            causes: Default::default(),
        }
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        let times: Vec<String> = self.frame_times.iter().map(|t| format!("{t:.1}")).collect();
        tracing::debug!(
            "settings window closed after {:.1} s: {} passes, {} painted (at {} s; asked for \
             by {:?})",
            self.opened.elapsed().as_secs_f32(),
            self.passes,
            self.painted,
            times.join(" "),
            self.causes
        );
    }
}

impl eframe::App for Settings {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.passes = self.passes.saturating_add(1);
        if let Some(after) = self.auto_close {
            let left = after.saturating_sub(self.opened.elapsed());
            if left.is_zero() {
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.painted = self.painted.saturating_add(1);
        if self.frame_times.len() < 200 {
            self.frame_times.push(self.opened.elapsed().as_secs_f32());
        }
        let causes = ui.ctx().repaint_causes();
        if causes.is_empty() {
            *self.causes.entry("a window event".into()).or_default() += 1;
        }
        for c in causes {
            *self.causes.entry(c.to_string()).or_default() += 1;
        }
        let status = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Devices");
            ui.add_space(4.0);
            egui::Grid::new("devices").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for device in [Device::Keyboard, Device::Headset] {
                    ui.strong(device.name());
                    ui.label(device_line(device, &status, SystemTime::now()));
                    ui.end_row();
                }
            });
        });
    }
}

/// "71%, 4078 mV, read 3 min ago", "asleep; last 91%, 4078 mV, read 2 h ago".
fn device_line(device: Device, status: &Status, now: SystemTime) -> String {
    let s = status.device(device);
    let off = if device == Device::Headset { "switched off" } else { "asleep" };
    // Status::device prefers the cable while it is plugged in.
    let cable = device == Device::Keyboard && status.keyboard_cable.state != State::Absent;
    let via = if cable { " (cable)" } else { "" };
    let reading = s.battery.as_ref().map(|b| {
        let age = now.duration_since(b.at).map(age).unwrap_or_else(|_| "just now".into());
        format!("{}, {} mV, read {age}", b.short(), b.millivolts)
    });
    match (&s.state, reading) {
        (State::Absent, _) => "not connected".into(),
        (State::Connected, Some(r)) => format!("{r}{via}"),
        (State::Connected, None) => format!("connected{via}"),
        (State::Sleeping, Some(r)) => format!("{off}; last {r}"),
        (State::Sleeping, None) => off.into(),
    }
}

/// "just now", "3 min ago", "2 h ago".
fn age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(Duration::from_secs(5)), "just now");
        assert_eq!(age(Duration::from_secs(185)), "3 min ago");
        assert_eq!(age(Duration::from_secs(7300)), "2 h ago");
    }

    #[test]
    fn a_sleeping_keyboard_shows_its_last_reading_and_age() {
        let mut s = Status::default();
        s.keyboard_receiver.state = State::Sleeping;
        let mut b = crate::battery::from_g915(&[0x0F, 0xEE, 0x00]).unwrap();
        let now = SystemTime::now();
        b.at = now - Duration::from_secs(7300);
        s.keyboard_receiver.battery = Some(b);
        assert_eq!(
            device_line(Device::Keyboard, &s, now),
            "asleep; last 91%, 4078 mV, read 2 h ago"
        );
        assert_eq!(device_line(Device::Headset, &s, now), "not connected");
    }

    #[test]
    fn the_keyboard_on_its_cable_says_so() {
        let mut s = Status::default();
        s.keyboard_cable.state = State::Connected;
        assert_eq!(device_line(Device::Keyboard, &s, SystemTime::now()), "connected (cable)");
    }
}
