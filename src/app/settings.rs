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
use std::time::{Duration, Instant};

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
use super::keyboard_editor;
use crate::config::Config;
use crate::ghub_import;
use crate::gkeys::GAction;
use crate::input::ButtonAction;
use crate::lighting::{Effect, Rgb, ZoneLight};
use crate::platform;

/// What the window starts from, and where it reports a save.
pub struct Editing {
    pub shared: Shared,
    /// The settings in use.
    pub config: Config,
    /// Why the file could not be read, if it could not: then nothing is
    /// saved over it.
    pub config_error: Option<String>,
    /// The tray window, told when the file has been saved.
    pub tray: isize,
}

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

/// Open the window and return once it has closed; `Ok(false)` if it was
/// already open and has been raised instead. `auto_close` closes it by
/// itself after that long (for measuring open/close cycles).
///
/// Must not be called while the app state is borrowed: the tray's messages
/// are dispatched while it runs.
pub fn run(editing: Editing, auto_close: Option<Duration>) -> Result<bool> {
    if is_open() {
        raise();
        return Ok(false);
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
            .with_inner_size([520.0, 680.0])
            .with_min_inner_size([400.0, 300.0])
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
            Ok(Box::new(Settings::new(editing, auto_close)))
        }),
    );
    CONTEXT.with(|c| *c.borrow_mut() = None);
    WINDOW.with(|w| w.set(None));
    RUNNING.with(|r| r.set(false));
    // Also when eframe failed before reaching the creator, after winit had
    // already registered.
    drop_raw_input();
    for (_, icon) in icons {
        if !icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
    result.map(|()| true).map_err(|e| anyhow!("settings window: {e}"))
}

struct Settings {
    shared: Shared,
    tray: isize,
    /// As in use, as edited, and why the file cannot be saved over.
    saved: Config,
    draft: Config,
    locked: Option<String>,
    autostart: bool,
    /// The outcome of the last save or change, shown under the buttons.
    note: Option<(String, bool)>,
    /// The lighting zones' colours as typed (sides, logo).
    hex: [String; 2],
    /// G HUB's per-key presets once read, and the one picked.
    ghub: Option<Result<GHub, String>>,
    ghub_pick: usize,
    tab: Tab,
    editor: keyboard_editor::Editor,
    /// The M bank shown in the G-key section.
    gkey_bank: usize,
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
    fn new(editing: Editing, auto_close: Option<Duration>) -> Self {
        Self {
            shared: editing.shared,
            tray: editing.tray,
            saved: editing.config.clone(),
            draft: editing.config,
            locked: editing.config_error,
            autostart: platform::starts_with_windows(),
            note: None,
            hex: Default::default(),
            ghub: None,
            ghub_pick: 0,
            tab: Tab::Settings,
            editor: Default::default(),
            gkey_bank: 0,
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
        super::devices::preview(None);
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
        // A frame can take more than one pass (a new grid sizes itself in
        // a second one); count frames.
        if ui.ctx().current_pass_index() == 0 {
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
        }
        let status = self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        egui::CentralPanel::default().show(ui, |ui| {
            let before = self.tab;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::Keyboard, "G915 layout");
            });
            if self.tab == Tab::Keyboard && before != Tab::Keyboard {
                // Room for the keyboard drawing.
                let size = ui.ctx().input(|i| i.viewport().inner_rect).map(|r| r.size());
                if let Some(s) = size
                    && s.x < 900.0
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                        920.0,
                        s.y.max(620.0),
                    )));
                }
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Settings => self.settings_tab(ui, &status),
                Tab::Keyboard => {
                    self.keyboard_lighting(ui);
                    ui.add_space(8.0);
                    self.editor.show(ui, &mut self.draft.keyboard.lighting.keys);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
            });
        });
        // The layout on the keyboard while it is being edited.
        let focused = ui.ctx().input(|i| i.viewport().focused).unwrap_or(true);
        let keys = &self.draft.keyboard.lighting.keys;
        let wanted =
            (self.tab == Tab::Keyboard && self.editor.preview && focused && !keys.is_empty())
                .then(|| keyboard_editor::frame(keys));
        self.editor.send_preview(ui.ctx(), wanted);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Settings,
    Keyboard,
}

impl Settings {
    fn settings_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("Devices");
        ui.add_space(4.0);
        egui::Grid::new("devices").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for device in [Device::Keyboard, Device::Headset] {
                ui.strong(device.name());
                ui.label(device_line(device, status));
                ui.end_row();
            }
        });
        ui.add_space(12.0);
        self.keyboard_gkeys(ui);
        ui.add_space(12.0);
        self.headset_buttons(ui);
        ui.add_space(12.0);
        self.headset_lighting(ui);
        ui.add_space(12.0);
        self.general(ui);
        ui.add_space(12.0);
        self.save_row(ui);
    }
}

/// G HUB's settings as read, and its per-key presets.
struct GHub {
    doc: serde_json::Value,
    presets: Vec<ghub_import::Preset>,
}

impl Settings {
    fn keyboard_lighting(&mut self, ui: &mut egui::Ui) {
        if self.draft.keyboard.lighting.keys.is_empty() {
            ui.label("No layout of blu-ghub's own yet: import one from G HUB to start from.");
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.editor.preview, "Show it on the keyboard while editing")
                .on_hover_text(
                    "Painted into the keyboard's memory only, while this window has the \
                     focus. The layout stored in the keyboard comes back by itself (when it \
                     dims or sleeps, or after a power cycle); nothing is stored.",
                );
        });
        ui.add_space(4.0);
        match &self.ghub {
            None => {
                if ui
                    .button("Read G HUB's layouts…")
                    .on_hover_text(
                        "Reads a copy of G HUB's settings; G HUB's own file is left as it is.",
                    )
                    .clicked()
                {
                    self.ghub = Some(read_ghub());
                    self.ghub_pick = 0;
                }
            }
            Some(Err(e)) => {
                ui.label(egui::RichText::new(e).color(ui.visuals().error_fg_color));
            }
            Some(Ok(g)) if g.presets.is_empty() => {
                ui.label("G HUB has no per-key layouts.");
            }
            Some(Ok(g)) => {
                let mut import = None;
                ui.horizontal(|ui| {
                    let pick = &mut self.ghub_pick;
                    egui::ComboBox::from_id_salt("ghub-preset")
                        .selected_text(g.presets.get(*pick).map_or("", |p| p.name.as_str()))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, p) in g.presets.iter().enumerate() {
                                ui.selectable_value(pick, i, &p.name);
                            }
                        });
                    if ui.button("Import").clicked() {
                        import = g.presets.get(*pick).map(|p| (p.name.clone(), p.id.clone()));
                    }
                });
                if let Some((name, id)) = import {
                    self.note = Some(match ghub_import::layout(&g.doc, &id) {
                        Ok(l) => {
                            if !l.skipped.is_empty() {
                                tracing::info!("G HUB import of {id}: skipped {:?}", l.skipped);
                            }
                            let n = l.keys.len();
                            self.draft.keyboard.lighting.keys = l.keys;
                            let skipped = match l.skipped.len() {
                                0 => String::new(),
                                s => format!(" ({s} entries without a G915 key left out)"),
                            };
                            (
                                format!(
                                    "Imported {n} keys from \"{name}\"{skipped}; Save keeps them."
                                ),
                                false,
                            )
                        }
                        Err(e) => (format!("Import: {e:#}"), true),
                    });
                }
            }
        }
    }

    fn keyboard_gkeys(&mut self, ui: &mut egui::Ui) {
        ui.heading("G915 G-keys");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (i, name) in ["M1", "M2", "M3"].into_iter().enumerate() {
                ui.selectable_value(&mut self.gkey_bank, i, name);
            }
        });
        let macros: Vec<String> = self.draft.macros.iter().map(|m| m.name.clone()).collect();
        let b = self.gkey_bank;
        let bank = self.draft.keyboard.gkeys.banks_mut().into_iter().nth(b);
        if let Some(bank) = bank {
            egui::Grid::new(("gkeys", b)).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (i, action) in bank.keys_mut().into_iter().enumerate() {
                    ui.strong(format!("G{}", i + 1));
                    ui.horizontal(|ui| gkey_editor(ui, (b, i), action, &macros));
                    ui.end_row();
                }
            });
        }
        ui.add_space(2.0);
        let hint = if self.draft.keyboard.gkeys.wanted() {
            "blu-ghub takes all five G-keys over (the keyboard hands over all or none); \
             those left to the keyboard still do what its own profile says."
        } else {
            "The keyboard keeps its G-keys and does what its own profile says."
        };
        ui.label(egui::RichText::new(hint).weak());
        if !self.draft.macros.is_empty() {
            ui.add_space(4.0);
            ui.label("Macros (written in BluGHub.toml for now):");
            for m in &self.draft.macros {
                let steps = m.steps.len();
                ui.label(format!(
                    "  {}: {}, {steps} step{}",
                    m.name,
                    m.mode.label(),
                    if steps == 1 { "" } else { "s" }
                ));
                for p in m.problems() {
                    ui.label(
                        egui::RichText::new(format!("    {p}")).color(ui.visuals().error_fg_color),
                    );
                }
            }
        }
    }

    fn headset_buttons(&mut self, ui: &mut egui::Ui) {
        ui.heading("G935 buttons");
        ui.add_space(4.0);
        let b = &mut self.draft.headset.buttons;
        egui::Grid::new("buttons").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            // In the order they sit on the ear cup, top to bottom (power
            // switch above, mute button and volume wheel below).
            for (name, slot) in [("G3", &mut b.g3), ("G2", &mut b.g2), ("G1", &mut b.g1)] {
                ui.strong(name);
                egui::ComboBox::from_id_salt(name)
                    .selected_text(slot.label())
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for a in ButtonAction::ALL {
                            ui.selectable_value(slot, a, a.label());
                        }
                    });
                ui.end_row();
            }
        });
        let actions = b.as_array();
        let taken = actions.iter().any(|a| *a != ButtonAction::Default);
        let hint = if !taken {
            "The headset keeps its buttons and mutes its microphone by itself."
        } else if actions.contains(&ButtonAction::Default) {
            "blu-ghub takes all three over, and with them the microphone mute (mute \
             button: toggle; boom up: mute; boom down: unmute). A button left to the \
             headset then does nothing."
        } else {
            "blu-ghub takes all three over, and with them the microphone mute (mute \
             button: toggle; boom up: mute; boom down: unmute)."
        };
        ui.add_space(2.0);
        ui.label(egui::RichText::new(hint).weak());
    }

    fn headset_lighting(&mut self, ui: &mut egui::Ui) {
        ui.heading("G935 lighting");
        ui.add_space(4.0);
        let l = &mut self.draft.headset.lighting;
        let [hex_sides, hex_logo] = &mut self.hex;
        egui::Grid::new("lighting").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for (name, zone, hex) in
                [("Sides", &mut l.sides, hex_sides), ("Logo", &mut l.logo, hex_logo)]
            {
                ui.strong(name);
                ui.vertical(|ui| zone_editor(ui, name, zone, hex));
                ui.end_row();
            }
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Set each time the headset switches on: it cannot store lighting itself.",
            )
            .weak(),
        );
    }

    fn general(&mut self, ui: &mut egui::Ui) {
        ui.heading("Batteries and logs");
        ui.add_space(4.0);
        let s = &mut self.draft.settings;
        egui::Grid::new("general").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label("Warn below");
            ui.add(egui::DragValue::new(&mut s.low_battery_percent).range(0..=100).suffix(" %"))
                .on_hover_text("A notification when a battery drops to this; 0 = never.");
            ui.end_row();
            ui.label("Read batteries every");
            ui.add(
                egui::DragValue::new(&mut s.battery_check_minutes).range(5..=1440).suffix(" min"),
            )
            .on_hover_text(
                "Only a fallback: both devices report changes by themselves. Nothing is \
                 sent to a device that is asleep or switched off.",
            );
            ui.end_row();
            ui.label("Keep logs for");
            ui.add(egui::DragValue::new(&mut s.log_retention_days).range(1..=3650).suffix(" days"))
                .on_hover_text("Takes effect at the next start.");
            ui.end_row();
        });
        ui.add_space(4.0);
        // Not part of the file: a per-user Run entry, changed at once.
        if ui.checkbox(&mut self.autostart, "Start with Windows").changed()
            && let Err(e) = platform::set_start_with_windows(self.autostart)
        {
            self.note = Some((format!("Start with Windows: {e:#}"), true));
            self.autostart = platform::starts_with_windows();
        }
    }

    fn save_row(&mut self, ui: &mut egui::Ui) {
        if let Some(why) = &self.locked {
            ui.label(
                egui::RichText::new(format!(
                    "The settings file could not be read, so it is not saved over; fix it \
                     by hand and restart BluGHub. ({why})"
                ))
                .color(ui.visuals().error_fg_color),
            );
            return;
        }
        let changed = self.draft != self.saved;
        ui.horizontal(|ui| {
            if ui.add_enabled(changed, egui::Button::new("Save")).clicked() {
                self.save();
            }
            if ui.add_enabled(changed, egui::Button::new("Undo changes")).clicked() {
                self.draft = self.saved.clone();
                self.note = None;
            }
        });
        if let Some((text, error)) = &self.note {
            let color =
                if *error { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
            ui.label(egui::RichText::new(text).color(color));
        }
    }

    fn save(&mut self) {
        let path = crate::app_paths::config_path();
        match self.draft.save(&path) {
            Ok(()) => {
                self.saved = self.draft.clone();
                self.note = Some(("Saved.".into(), false));
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(self.tray as *mut _)),
                        super::WM_APP_CONFIG,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            Err(e) => self.note = Some((format!("Not saved: {e:#}"), true)),
        }
    }
}

fn read_ghub() -> Result<GHub, String> {
    let path = ghub_import::settings_path().ok_or("LOCALAPPDATA is not set")?;
    if !path.exists() {
        return Err(format!("G HUB's settings are not at {}", path.display()));
    }
    let doc = ghub_import::read(&path).map_err(|e| format!("{e:#}"))?;
    let presets = ghub_import::presets(&doc);
    Ok(GHub { doc, presets })
}

/// One G-key: left to the keyboard, nothing, text, a media key or a macro.
fn gkey_editor(ui: &mut egui::Ui, id: (usize, usize), action: &mut GAction, macros: &[String]) {
    const KINDS: [&str; 5] =
        ["The keyboard's own", "Nothing", "Type text", "Media or volume key", "Macro"];
    let kind = |a: &GAction| match a {
        GAction::Default => 0,
        GAction::Off => 1,
        GAction::Text(_) => 2,
        GAction::Media(_) => 3,
        GAction::Macro(_) => 4,
    };
    let mut k = kind(action);
    egui::ComboBox::from_id_salt(("gkind", id)).selected_text(KINDS[k]).width(160.0).show_ui(
        ui,
        |ui| {
            for (i, label) in KINDS.iter().enumerate() {
                ui.selectable_value(&mut k, i, *label);
            }
        },
    );
    if k != kind(action) {
        *action = match k {
            0 => GAction::Default,
            1 => GAction::Off,
            2 => GAction::Text(String::new()),
            3 => GAction::Media(ButtonAction::PlayPause),
            _ => GAction::Macro(macros.first().cloned().unwrap_or_default()),
        };
    }
    match action {
        GAction::Text(t) => {
            ui.add(egui::TextEdit::singleline(t).desired_width(200.0).hint_text("text to type"));
        }
        GAction::Media(m) => {
            egui::ComboBox::from_id_salt(("gmedia", id))
                .selected_text(m.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for a in ButtonAction::ALL {
                        if !matches!(a, ButtonAction::Default | ButtonAction::Off) {
                            ui.selectable_value(m, a, a.label());
                        }
                    }
                });
        }
        GAction::Macro(_) if macros.is_empty() => {
            ui.label(egui::RichText::new("no macros in BluGHub.toml yet").weak());
        }
        GAction::Macro(name) => {
            egui::ComboBox::from_id_salt(("gmacro", id))
                .selected_text(name.as_str())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for m in macros {
                        ui.selectable_value(name, m.clone(), m);
                    }
                });
        }
        GAction::Default | GAction::Off => {}
    }
}

/// One zone: left to the headset, or an effect with its colour, speed
/// and brightness.
fn zone_editor(ui: &mut egui::Ui, id: &str, zone: &mut Option<ZoneLight>, hex: &mut String) {
    let text = zone.map_or("Left to the headset", |z| z.effect.label());
    egui::ComboBox::from_id_salt(id).selected_text(text).width(150.0).show_ui(ui, |ui| {
        if ui.selectable_label(zone.is_none(), "Left to the headset").clicked() {
            *zone = None;
        }
        for e in Effect::ALL {
            let chosen = zone.is_some_and(|z| z.effect == e);
            if ui.selectable_label(chosen, e.label()).clicked() {
                *zone = Some(ZoneLight { effect: e, ..zone.unwrap_or_default() });
            }
        }
    });
    let Some(z) = zone else { return };
    if z.effect.has_color() {
        ui.horizontal(|ui| {
            ui.label("Colour");
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut z.color.0);
            let typed = ui.add(
                egui::TextEdit::singleline(hex)
                    .desired_width(70.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("#rrggbb"),
            );
            if typed.changed()
                && let Ok(c) = hex.parse::<Rgb>()
            {
                z.color = c;
            }
            // Follows the picker, except while being typed in.
            if !typed.has_focus() {
                *hex = z.color.to_string();
            }
        });
    }
    if z.effect.has_period() {
        ui.horizontal(|ui| {
            ui.label("Speed");
            let mut seconds = z.period_ms as f64 / 1000.0;
            let range = *ZoneLight::PERIOD_MS.start() as f64 / 1000.0
                ..=*ZoneLight::PERIOD_MS.end() as f64 / 1000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut seconds)
                        .range(range)
                        .speed(0.05)
                        .max_decimals(3)
                        .suffix(" s"),
                )
                .on_hover_text(
                    "One breath, or one round of the colours: 0.001 to 65.535 s, the most the \
                     headset's setting can hold. Drag, or double-click to type.",
                )
                .changed()
            {
                z.period_ms = (seconds * 1000.0).round().clamp(1.0, u16::MAX as f64) as u16;
            }
        });
    }
    if z.effect != Effect::Off {
        ui.horizontal(|ui| {
            ui.label("Brightness");
            ui.add(egui::Slider::new(&mut z.brightness, 1..=100).suffix(" %"));
        });
    }
}

/// "71%, 4078 mV, read at 14:32", "asleep; last 91%, 4078 mV, read at 09:05".
/// The time of day rather than an age, which would go stale in a window
/// that draws nothing until something changes.
fn device_line(device: Device, status: &Status) -> String {
    let s = status.device(device);
    let off = if device == Device::Headset { "switched off" } else { "asleep" };
    // Status::device prefers the cable while it is plugged in.
    let cable = device == Device::Keyboard && status.keyboard_cable.state != State::Absent;
    let via = if cable { " (cable)" } else { "" };
    let reading = s.battery.as_ref().map(|b| {
        let at = chrono::DateTime::<chrono::Local>::from(b.at).format("%H:%M");
        format!("{}, {} mV, read at {at}", b.short(), b.millivolts)
    });
    match (&s.state, reading) {
        (State::Absent, _) => "not connected".into(),
        (State::Connected, Some(r)) => format!("{r}{via}"),
        (State::Connected, None) => format!("connected{via}"),
        (State::Sleeping, Some(r)) => format!("{off}; last {r}"),
        (State::Sleeping, None) => off.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sleeping_keyboard_shows_its_last_reading_and_when() {
        let mut s = Status::default();
        s.keyboard_receiver.state = State::Sleeping;
        let b = crate::battery::from_g915(&[0x0F, 0xEE, 0x00]).unwrap();
        let at = chrono::DateTime::<chrono::Local>::from(b.at).format("%H:%M").to_string();
        s.keyboard_receiver.battery = Some(b);
        assert_eq!(
            device_line(Device::Keyboard, &s),
            format!("asleep; last 91%, 4078 mV, read at {at}")
        );
        assert_eq!(device_line(Device::Headset, &s), "not connected");
    }

    #[test]
    fn the_keyboard_on_its_cable_says_so() {
        let mut s = Status::default();
        s.keyboard_cable.state = State::Connected;
        assert_eq!(device_line(Device::Keyboard, &s), "connected (cable)");
    }
}
