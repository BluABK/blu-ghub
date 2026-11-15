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
use crate::config::{Config, ShowLayout, Sidetone};
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
            .with_inner_size([860.0, 640.0])
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
    /// Serial numbers and wireless ids shown unmasked, until the window
    /// closes.
    show_ids: bool,
    /// The equaliser band being dragged.
    eq_drag: Option<usize>,
    /// The name typed for saving the equaliser as a preset, while saving.
    eq_preset_name: Option<String>,
    macro_editor: super::macro_editor::MacroEditor,
    /// An open file dialog: for which (bank, key), and its answer.
    browsing: Option<((usize, usize), FileAnswer)>,
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
            tab: Tab::Overview,
            editor: Default::default(),
            gkey_bank: 0,
            show_ids: false,
            eq_drag: None,
            eq_preset_name: None,
            macro_editor: Default::default(),
            browsing: None,
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
        super::devices::preview_eq(None);
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
                let kb = Device::Keyboard.label();
                ui.selectable_value(&mut self.tab, Tab::Overview, "Overview");
                ui.selectable_value(&mut self.tab, Tab::GKeys, format!("{kb} G-keys"));
                ui.selectable_value(&mut self.tab, Tab::Layout, format!("{kb} layout"));
                ui.selectable_value(&mut self.tab, Tab::Macros, "Macros");
                ui.selectable_value(&mut self.tab, Tab::Headset, Device::Headset.label());
            });
            if self.tab == Tab::Layout && before != Tab::Layout {
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
                Tab::Overview => {
                    self.overview_tab(ui, &status);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
                Tab::GKeys => {
                    self.keyboard_gkeys(ui);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
                Tab::Headset => {
                    ui.columns(2, |c| {
                        self.headset_buttons(&mut c[0]);
                        c[0].add_space(12.0);
                        self.headset_sound(&mut c[0]);
                        self.headset_lighting(&mut c[1]);
                    });
                    ui.add_space(12.0);
                    self.headset_eq(ui, &status);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
                Tab::Layout => {
                    self.keyboard_lighting(ui);
                    ui.add_space(8.0);
                    self.keyboard_power(ui);
                    ui.add_space(8.0);
                    self.editor.show(ui, &mut self.draft.keyboard.lighting.keys);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
                Tab::Macros => {
                    let d = &mut self.draft;
                    self.macro_editor.show(ui, &mut d.macros, &mut d.keyboard.gkeys);
                    ui.add_space(12.0);
                    self.save_row(ui);
                }
            });
        });
        // The layout on the keyboard while it is being edited, if it is the
        // one to be shown: with "its stored layout" picked, the keyboard
        // shows what Save will give.
        let focused = ui.ctx().input(|i| i.viewport().focused).unwrap_or(true);
        let l = &self.draft.keyboard.lighting;
        let wanted = (self.tab == Tab::Layout
            && self.editor.preview
            && focused
            && l.show == ShowLayout::Layout
            && !l.keys.is_empty())
        .then(|| l.frame());
        self.editor.send_preview(ui.ctx(), wanted);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Both devices side by side, with their details; batteries and logs.
    Overview,
    GKeys,
    Layout,
    Macros,
    Headset,
}

impl Settings {
    fn overview_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.columns(2, |c| {
            for (col, device) in c.iter_mut().zip([Device::Keyboard, Device::Headset]) {
                col.heading(device.label());
                col.label(device_line(device, status));
                col.add_space(6.0);
                let d = &status.device(device).details;
                if d.firmware.is_empty() && d.sidetone.is_none() {
                    col.label(egui::RichText::new("Details once it connects.").weak());
                } else {
                    self.details(col, device, d);
                }
            }
        });
        ui.add_space(12.0);
        self.general(ui);
    }
}

/// G HUB's settings as read, and its per-key presets.
struct GHub {
    doc: serde_json::Value,
    presets: Vec<ghub_import::Preset>,
}

impl Settings {
    /// Dimming when idle, and when the lighting goes off.
    fn keyboard_power(&mut self, ui: &mut egui::Ui) {
        use crate::config::{Dim, KeyboardPower};
        let p = &mut self.draft.keyboard.power;
        ui.horizontal(|ui| {
            let mut on = p.dim.is_some();
            if ui
                .checkbox(&mut on, "Dim after")
                .on_hover_text(
                    "blu-ghub's layout fades once nothing (keyboard or mouse) was touched for \
                     this long, and is bright again at the next touch. With its stored layout \
                     the keyboard dims by itself after this long without a key, to its own \
                     level, at once.",
                )
                .changed()
            {
                p.dim = on.then(Dim::default);
            }
            let mut d = p.dim.unwrap_or_default();
            ui.add_enabled_ui(on, |ui| {
                ui.add(egui::DragValue::new(&mut d.after).range(KeyboardPower::AFTER).suffix(" s"));
                ui.label("idle, fading over");
                ui.add(
                    egui::DragValue::new(&mut d.fade)
                        .range(KeyboardPower::FADE)
                        .speed(0.1)
                        .max_decimals(1)
                        .suffix(" s"),
                );
                ui.label("to");
                ui.add(egui::DragValue::new(&mut d.to).range(0..=100).suffix(" %"))
                    .on_hover_text("Of the layout's own brightness; 0 is dark.");
            });
            if on {
                p.dim = Some(d);
            }
        });
        ui.horizontal(|ui| {
            let mut on = p.lights_off_after.is_some();
            if ui
                .checkbox(&mut on, "Lighting off after")
                .on_hover_text(
                    "The keyboard's own sleep: its lighting goes off this long after the last \
                     key, and comes back at the next. Unticked: the keyboard's own time (5 \
                     minutes on this model), which it also goes back to when switched off and \
                     on.",
                )
                .changed()
            {
                p.lights_off_after = on.then_some(300);
            }
            let mut s = p.lights_off_after.unwrap_or(300);
            ui.add_enabled_ui(on, |ui| {
                ui.add(egui::DragValue::new(&mut s).range(KeyboardPower::LIGHTS_OFF).suffix(" s"));
                ui.label("without a key");
            });
            if on {
                p.lights_off_after = Some(s);
            }
        });
        if let (Some(d), Some(off)) = (p.dim, p.lights_off_after)
            && d.after >= off
        {
            ui.label(
                egui::RichText::new("The lighting goes off before it would dim.")
                    .color(ui.visuals().warn_fg_color),
            );
        }
    }

    fn keyboard_lighting(&mut self, ui: &mut egui::Ui) {
        if self.draft.keyboard.lighting.keys.is_empty() {
            ui.label("No layout of blu-ghub's own yet: import one from G HUB to start from.");
        }
        let l = &mut self.draft.keyboard.lighting;
        ui.horizontal(|ui| {
            ui.strong("The keyboard shows:");
            ui.radio_value(&mut l.show, ShowLayout::Stored, "its stored layout")
                .on_hover_text("blu-ghub sends the keyboard nothing.");
            ui.radio_value(&mut l.show, ShowLayout::Layout, "this layout").on_hover_text(
                "Painted whenever the keyboard connects or wakes, into its memory only, with \
                 its own effects held off meanwhile: it dims only as \"Dim after\" says, and \
                 still goes dark when it sleeps. Without blu-ghub running, the keyboard shows \
                 its stored layout.",
            );
        });
        let saved = self.saved.keyboard.lighting.show;
        let note = match (saved, l.show) {
            (ShowLayout::Stored, ShowLayout::Stored) => {
                "Away from this tab the keyboard shows its stored layout, not this one."
            }
            (ShowLayout::Layout, ShowLayout::Layout) => {
                "The keyboard shows this layout as last saved, also with this window closed."
            }
            _ => "Save to make the keyboard keep this choice.",
        };
        ui.label(egui::RichText::new(note).weak());
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.editor.preview, "Show the changes on the keyboard while editing")
                .on_hover_text(
                    "While this tab is open, the window has the focus and \"this layout\" is \
                     picked. Painted into the keyboard's memory only; leaving the tab shows the \
                     saved choice again.",
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
        ui.heading(format!("{} G-keys", Device::Keyboard.label()));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (i, name) in ["M1", "M2", "M3"].into_iter().enumerate() {
                ui.selectable_value(&mut self.gkey_bank, i, name);
            }
        });
        let macros: Vec<String> = self.draft.macros.iter().map(|m| m.name.clone()).collect();
        let b = self.gkey_bank;
        let bank = self.draft.keyboard.gkeys.banks_mut().into_iter().nth(b);
        let mut browse = None;
        if let Some(bank) = bank {
            egui::Grid::new(("gkeys", b)).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (i, action) in bank.keys_mut().into_iter().enumerate() {
                    ui.strong(format!("G{}", i + 1));
                    if ui.horizontal(|ui| gkey_editor(ui, (b, i), action, &macros)).inner {
                        browse = Some((b, i));
                    }
                    ui.end_row();
                }
            });
        }
        if let Some(at) = browse
            && self.browsing.is_none()
        {
            self.browsing = Some((at, pick_file(ui.ctx().clone())));
        }
        // The file dialog's answer.
        if let Some(((b, i), rx)) = &self.browsing {
            match rx.try_recv() {
                Ok(picked) => {
                    let (b, i) = (*b, *i);
                    self.browsing = None;
                    if let Some(path) = picked
                        && let Some(bank) = self.draft.keyboard.gkeys.banks_mut().into_iter().nth(b)
                        && let Some(slot) = bank.keys_mut().into_iter().nth(i)
                    {
                        *slot = GAction::PasteFile(path);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.browsing = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.add_space(4.0);
        let g = &mut self.draft.keyboard.gkeys;
        ui.horizontal(|ui| {
            ui.checkbox(&mut g.show_bank, "Show the bank on G1-G5:").on_hover_text(
                "While blu-ghub has the G-keys, the M keys' own lights stay dark, so G1-G5 \
                 light up in the bank's colour instead. This holds the keyboard's own lighting \
                 effects off (it no longer dims after a minute; it still sleeps).",
            );
            for (i, name) in ["M1", "M2", "M3"].into_iter().enumerate() {
                ui.label(name);
                egui::widgets::color_picker::color_edit_button_srgb(ui, &mut g.bank_colors[i].0);
            }
        });
        ui.checkbox(&mut g.light_m_keys, "Light the M keys").on_hover_text(
            "The keyboard only lets blu-ghub light them in host mode, so while blu-ghub has \
             the G-keys the keyboard is switched to host mode, and back to on-board mode when \
             blu-ghub lets go. If blu-ghub is killed meanwhile, the G and M keys do nothing \
             until it starts again or the keyboard is switched off and on; typing and lighting \
             are not affected.",
        );
        ui.add_space(2.0);
        let hint = if self.draft.keyboard.gkeys.wanted() {
            "blu-ghub takes all five G-keys over (the keyboard hands over all or none); \
             those left to the keyboard still do what its own profile says."
        } else {
            "The keyboard keeps its G-keys and does what its own profile says."
        };
        ui.label(egui::RichText::new(hint).weak());
        let problems: usize = self.draft.macros.iter().map(|m| m.problems().len()).sum();
        if problems > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "{problems} problem(s) in the macros: see the Macros tab."
                ))
                .color(ui.visuals().error_fg_color),
            );
        }
    }

    fn headset_buttons(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("{} buttons", Device::Headset.label()));
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

    /// What a device says about itself. Its serial number and wireless id
    /// are masked until "Show" is clicked, so a screenshot can be shared.
    fn details(&mut self, ui: &mut egui::Ui, device: Device, d: &super::details::Details) {
        let masked = |v: &str| "\u{2022}".repeat(v.chars().count().max(8));
        egui::Grid::new(("details-grid", device.name())).num_columns(2).spacing([16.0, 4.0]).show(
            ui,
            |ui| {
                for (n, f) in d.firmware.iter().enumerate() {
                    ui.label(if n == 0 { "Firmware" } else { "" });
                    ui.label(f);
                    ui.end_row();
                }
                if let Some(f) = &d.receiver_firmware {
                    ui.label("Receiver firmware");
                    ui.label(f);
                    ui.end_row();
                }
                for (what, value) in [("Serial number", &d.serial), ("Wireless id", &d.wireless_id)]
                {
                    let Some(v) = value else { continue };
                    ui.label(what);
                    ui.horizontal(|ui| {
                        ui.monospace(if self.show_ids { v.clone() } else { masked(v) });
                        let text = if self.show_ids { "Hide" } else { "Show" };
                        if ui
                            .small_button(text)
                            .on_hover_text("Shown only until the window closes.")
                            .clicked()
                        {
                            self.show_ids = !self.show_ids;
                        }
                    });
                    ui.end_row();
                }
                if let Some(ms) = d.profile_report_ms {
                    ui.label("Report rate");
                    ui.label(format!("{ms} ms, {} Hz (the active profile's)", 1000 / ms as u32));
                    ui.end_row();
                }
                if let Some(ms) = d.report_ms {
                    ui.label("Receiver's record");
                    ui.label(format!("{ms} ms (pairing record)")).on_hover_text(
                        "Byte 2 of the receiver's pairing record for the keyboard, which Solaar \
                         shows as its polling rate. The keyboard's own report rate is the row \
                         above.",
                    );
                    ui.end_row();
                }
                if let Some((now, max)) = d.backlight {
                    ui.label("Backlight");
                    ui.label(format!("{now} of {max}"));
                    ui.end_row();
                }
                if let Some(p) = &d.profile {
                    ui.label("Bank");
                    ui.label(p);
                    ui.end_row();
                }
                if let Some(s) = d.sidetone {
                    ui.label("Sidetone");
                    ui.label(format!("{s} of 100"));
                    ui.end_row();
                }
                if let Some(bands) = &d.eq {
                    ui.label("Equaliser");
                    ui.vertical(|ui| {
                        for row in bands.chunks(5) {
                            let text: Vec<String> = row
                                .iter()
                                .map(|(hz, db)| {
                                    // + 0.0: no "-0.0".
                                    let db = db + 0.0;
                                    format!("{} {db:+.1} dB", super::details::band_name(*hz))
                                })
                                .collect();
                            ui.label(text.join(",  "));
                        }
                    });
                    ui.end_row();
                }
                if device == Device::Headset {
                    ui.label("Microphone");
                    ui.label(match (d.mic_handled, d.mic_muted) {
                        (false, _) => "muted by the headset itself, which tells nobody",
                        (true, Some(true)) => "muted (in Windows)",
                        (true, Some(false)) => "on",
                        (true, None) => "not known yet",
                    });
                    ui.end_row();
                    ui.label("Boom");
                    ui.label(match (d.mic_handled, d.boom_up) {
                        (false, _) => "not reported while the headset has its buttons",
                        (true, Some(true)) => "up",
                        (true, Some(false)) => "down",
                        (true, None) => "not moved since blu-ghub took the buttons",
                    });
                    ui.end_row();
                }
            },
        );
    }

    fn headset_sound(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("{} sound", Device::Headset.label()));
        ui.add_space(4.0);
        let h = &mut self.draft.headset;
        ui.horizontal(|ui| {
            let mut on = h.sidetone.is_some_and(|s| s.on);
            if ui
                .checkbox(&mut on, "Sidetone")
                .on_hover_text(
                    "How much of your own voice the headset plays back, set each time it \
                     switches on. Unticked: off (0); the level is kept for when it is ticked \
                     again.",
                )
                .changed()
            {
                match &mut h.sidetone {
                    Some(s) => s.on = on,
                    None => h.sidetone = Some(Sidetone { on, level: 0 }),
                }
            }
            if let Some(s) = &mut h.sidetone {
                ui.add_enabled(s.on, egui::Slider::new(&mut s.level, 0..=100));
            }
        });
    }

    /// The equaliser Windows plays the headset through (Logitech's audio
    /// processing), as a curve. Unticked it is shown and left alone;
    /// ticked, a dragged band is written when let go, and Save keeps it.
    fn headset_eq(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading(format!("{} equaliser", Device::Headset.label()));
        ui.add_space(4.0);
        let Some(bands) = status.headset.details.eq.as_ref() else {
            ui.label(
                egui::RichText::new(
                    "No Logitech equaliser on the headset's sound in Windows (it comes with \
                     Logitech's audio driver).",
                )
                .weak(),
            );
            return;
        };
        let freqs: Vec<f32> = bands.iter().map(|b| b.0).collect();
        let found: Vec<f32> = bands.iter().map(|b| b.1).collect();
        let h = &mut self.draft.headset;
        ui.horizontal(|ui| {
            let mut on = h.equaliser.is_some();
            if ui
                .checkbox(&mut on, "Set the equaliser")
                .on_hover_text(
                    "Drag a point up or down; it is written when let go. Double-click a point \
                     for 0 dB. Save keeps it. Unticked, it is left as it is.",
                )
                .changed()
            {
                h.equaliser = on.then(|| found.clone());
                if !on {
                    super::devices::preview_eq(None);
                }
            }
            let Some(g) = &mut h.equaliser else { return };
            use super::eq_presets;
            let now = eq_presets::matching(g, &freqs, &h.eq_presets);
            let mut pick: Option<Vec<f32>> = None;
            egui::ComboBox::from_id_salt("eq-preset")
                .selected_text(now.as_deref().unwrap_or("Your own curve"))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for s in &eq_presets::STANDARD {
                        let on = now.as_deref() == Some(s.name);
                        if ui.selectable_label(on, s.name).on_hover_text(s.about).clicked() {
                            pick = Some(s.gains(&freqs));
                        }
                    }
                    if !h.eq_presets.is_empty() {
                        ui.separator();
                    }
                    for p in &h.eq_presets {
                        let on = now.as_deref() == Some(p.name.as_str());
                        if ui.selectable_label(on, &p.name).clicked() {
                            pick = Some(p.gains.clone());
                        }
                    }
                })
                .response
                .on_hover_text("blu-ghub's presets, then your own.");
            if let Some(p) = pick {
                *g = p;
                super::devices::preview_eq(Some(g.clone()));
            }
            let own = now.as_ref().filter(|n| !eq_presets::is_standard(n));
            if self.eq_preset_name.is_none() {
                if now.is_none()
                    && ui
                        .button("Save as preset")
                        .on_hover_text("Keep this curve by name, in the list beside blu-ghub's.")
                        .clicked()
                {
                    self.eq_preset_name = Some(String::new());
                }
                if let Some(name) = own
                    && ui.button("Delete preset").on_hover_text("Save keeps it deleted.").clicked()
                {
                    h.eq_presets.retain(|p| &p.name != name);
                }
            }
        });
        if let (Some(name), Some(g)) = (&mut self.eq_preset_name, &h.equaliser) {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label("Preset name");
                let field = ui.add(egui::TextEdit::singleline(name).desired_width(160.0));
                if field.gained_focus() || name.is_empty() {
                    field.request_focus();
                }
                let trimmed = name.trim().to_owned();
                let clash = super::eq_presets::is_standard(&trimmed);
                let replaces = h.eq_presets.iter().any(|p| p.name.eq_ignore_ascii_case(&trimmed));
                let ok = !trimmed.is_empty() && !clash;
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .add_enabled(ok, egui::Button::new(if replaces { "Replace" } else { "Keep" }))
                    .clicked()
                    || (ok && enter)
                {
                    h.eq_presets.retain(|p| !p.name.eq_ignore_ascii_case(&trimmed));
                    h.eq_presets.push(crate::config::EqPreset { name: trimmed, gains: g.clone() });
                    done = true;
                } else if ui.button("Cancel").clicked() {
                    done = true;
                } else if clash {
                    ui.label(
                        egui::RichText::new("That is one of blu-ghub's.")
                            .color(ui.visuals().warn_fg_color),
                    );
                } else {
                    ui.label(egui::RichText::new("Save keeps it in the config.").weak());
                }
            });
            if done {
                self.eq_preset_name = None;
            }
        }
        let range = (-12.0, 12.0);
        match &mut h.equaliser {
            Some(g) => {
                g.resize(freqs.len(), 0.0);
                let r = super::eq_widget::show(ui, &freqs, g, range, 0.5, true, &mut self.eq_drag);
                if r.settled {
                    super::devices::preview_eq(Some(g.clone()));
                }
            }
            None => {
                let mut g = found;
                super::eq_widget::show(ui, &freqs, &mut g, range, 0.5, false, &mut None);
            }
        }
        ui.label(
            egui::RichText::new(
                "This is the equaliser in Logitech's audio processing in Windows, which the \
                 headset's sound goes through. A change is heard from the next time playback \
                 starts: stop and play (pausing is not enough).",
            )
            .weak(),
        );
    }

    fn headset_lighting(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("{} lighting", Device::Headset.label()));
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
                super::devices::preview_eq(None);
                self.note = None;
            }
            if ui
                .button("Re-apply to the devices")
                .on_hover_text(
                    "Hands everything back to the keyboard and headset and takes it again, \
                     painting from nothing, with the saved settings: for when a device shows \
                     something other than they say. Save applies changes without this.",
                )
                .clicked()
            {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(self.tray as *mut _)),
                        super::WM_APP_REAPPLY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                self.note = Some(("Re-applied to the devices.".into(), false));
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

/// One G-key: left to the keyboard, nothing, text (typed or pasted), a
/// file to paste, a media key or a macro. `true` when "Browse…" was
/// clicked.
fn gkey_editor(
    ui: &mut egui::Ui,
    id: (usize, usize),
    action: &mut GAction,
    macros: &[String],
) -> bool {
    const KINDS: [&str; 8] = [
        "The keyboard's own",
        "Nothing",
        "A key (held as long)",
        "Type text",
        "Paste text",
        "Paste a file or image",
        "Media or volume key",
        "Macro",
    ];
    let kind = |a: &GAction| match a {
        GAction::Default => 0,
        GAction::Off => 1,
        GAction::Key(_) => 2,
        GAction::Text(_) => 3,
        GAction::Paste(_) => 4,
        GAction::PasteFile(_) => 5,
        GAction::Media(_) => 6,
        GAction::Macro(_) => 7,
    };
    let mut browse = false;
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
            2 => GAction::Key("left_ctrl".into()),
            3 => GAction::Text(String::new()),
            4 => GAction::Paste(String::new()),
            5 => GAction::PasteFile(Default::default()),
            6 => GAction::Media(ButtonAction::PlayPause),
            _ => GAction::Macro(macros.first().cloned().unwrap_or_default()),
        };
    }
    match action {
        GAction::Key(s) => {
            let mut c = crate::gkeys::Combo::parse(s);
            let before = c.clone();
            for (on, name) in [
                (&mut c.ctrl, "Ctrl"),
                (&mut c.shift, "Shift"),
                (&mut c.alt, "Alt"),
                (&mut c.win, "Win"),
            ] {
                ui.checkbox(on, name);
            }
            egui::ComboBox::from_id_salt(("gkey-key", id))
                .selected_text(if c.key.is_empty() { "(none)" } else { c.key.as_str() })
                .width(130.0)
                .height(300.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut c.key, String::new(), "(none)");
                    for name in super::macro_editor::key_names() {
                        ui.selectable_value(&mut c.key, name.to_owned(), name);
                    }
                })
                .response
                .on_hover_text(
                    "Held for as long as the G-key is. F13-F24 are not on the keyboard, so \
                     programs can take them for hotkeys of their own.",
                );
            if c != before {
                *s = c.text();
            }
        }
        GAction::Text(t) => {
            ui.add(egui::TextEdit::singleline(t).desired_width(200.0).hint_text("text to type"))
                .on_hover_text("Typed as if from a keyboard, whatever its layout.");
        }
        GAction::Paste(t) => {
            ui.add(egui::TextEdit::singleline(t).desired_width(200.0).hint_text("text to paste"))
                .on_hover_text(
                    "Put on the clipboard and pasted with Ctrl+V; what was on the clipboard \
                     goes back after. For programs that drop typed text (Windows 11's \
                     Notepad).",
                );
        }
        GAction::PasteFile(p) => {
            let mut text = p.to_string_lossy().into_owned();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .desired_width(200.0)
                        .hint_text("file to paste"),
                )
                .on_hover_text(
                    "Pasted as a file, as Explorer's Copy would (chat programs upload it); an \
                     image also as a picture. What was on the clipboard goes back after.",
                )
                .changed()
            {
                *p = text.into();
            }
            browse = ui.button("Browse…").clicked();
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
            ui.label(egui::RichText::new("no macros yet: make one in the Macros tab").weak());
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
    browse
}

/// The file dialog's answer; `None`: cancelled.
pub(super) type FileAnswer = std::sync::mpsc::Receiver<Option<std::path::PathBuf>>;

/// The file dialog, on a thread of its own: its modal loop would run the
/// settings window's messages from inside a frame otherwise. The answer
/// comes back through the receiver.
pub(super) fn pick_file(ctx: egui::Context) -> FileAnswer {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };
        use windows::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let mut file = vec![0u16; 1024];
        let filter: Vec<u16> = "Images\0*.png;*.jpg;*.jpeg;*.gif;*.bmp;*.webp;*.tif;*.tiff;*.ico\0\
                                All files\0*.*\0\0"
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "File to paste\0".encode_utf16().collect();
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
            lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
            nMaxFile: file.len() as u32,
            lpstrTitle: windows::core::PCWSTR(title.as_ptr()),
            Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
            ..Default::default()
        };
        let picked = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool().then(|| {
            let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
            std::path::PathBuf::from(String::from_utf16_lossy(&file[..end]))
        });
        if com {
            unsafe { CoUninitialize() };
        }
        let _ = tx.send(picked);
        ctx.request_repaint();
    });
    rx
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
        format!("{}, {} mV, read at {at}", b.shown(), b.millivolts)
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
            format!("asleep; last \u{1F50B} 91%, 4078 mV, read at {at}")
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
