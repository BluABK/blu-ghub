//! Diagnostic tools: evidence gathering before (and alongside) the real app.
//!
//! Each writes its findings both to the console and to a timestamped file
//! under `%APPDATA%\BluGHub\diag\`, so a run can be left going and read later.

pub mod caps;
pub mod keys;
pub mod light;
pub mod onboard;
pub mod probe;
pub mod slots;
pub mod sniff;

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, ErrorKind, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::{CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
use windows::core::BOOL;

use crate::app_paths;

/// Set once Ctrl+C (or closing the console) asks the tool to stop.
pub static STOP: AtomicBool = AtomicBool::new(false);
/// An event to signal on stop, for tools that block in a wait (0 = none).
static STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
/// A window to send `WM_CLOSE` on stop, for tools that run a message loop.
static STOP_HWND: AtomicIsize = AtomicIsize::new(0);

/// Route Ctrl+C, Ctrl+Break and closing the console window to a clean stop,
/// so every tool still writes its final summary.
///
/// Logging off and shutting down are *not* covered: a console program that
/// has loaded user32 (all of these have) never receives those two events, so
/// such a run ends without a final summary. The hourly summaries and every
/// line before them are already on disk.
pub fn install_stop_handler(event: Option<HANDLE>, hwnd: Option<HWND>) {
    if let Some(e) = event {
        STOP_EVENT.store(e.0 as isize, Ordering::SeqCst);
    }
    if let Some(h) = hwnd {
        STOP_HWND.store(h.0 as isize, Ordering::SeqCst);
    }
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_ctrl), true);
    }
}

unsafe extern "system" fn on_console_ctrl(kind: u32) -> BOOL {
    STOP.store(true, Ordering::SeqCst);
    let e = STOP_EVENT.load(Ordering::SeqCst);
    if e != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(e as *mut _));
        }
    }
    let h = STOP_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    // Closing the console window ends the process as soon as this handler
    // returns; give the main thread time to write its summary.
    if kind != CTRL_C_EVENT {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    BOOL(1)
}

/// A sink that writes each line to a file and to the console.
///
/// The file comes first and the console is best effort: once the console is
/// gone (window closed, pipe reader exited) the rest of the run still reaches
/// the file instead of aborting on a failed print.
pub struct Tee {
    file: Option<BufWriter<File>>,
    console_ok: bool,
    pub path: PathBuf,
}

impl Tee {
    /// Opens `%APPDATA%\BluGHub\diag\<prefix>-<timestamp>.log`, never
    /// overwriting an existing file: a second run started in the same second
    /// gets `-2`, `-3`, ... rather than emptying the first run's evidence.
    pub fn create(prefix: &str) -> Self {
        let dir = app_paths::diag_dir();
        app_paths::ensure_dir(&dir);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let mut path = dir.join(format!("{prefix}-{stamp}.log"));
        let mut file = None;
        for n in 1..=20 {
            if n > 1 {
                path = dir.join(format!("{prefix}-{stamp}-{n}.log"));
            }
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => {
                    file = Some(BufWriter::new(f));
                    break;
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    let _ = writeln!(
                        io::stderr(),
                        "could not create {}: {e} (console output only)",
                        path.display()
                    );
                    break;
                }
            }
        }
        Self { file, console_ok: true, path }
    }

    /// A tee that writes nowhere, for tests.
    pub fn sink() -> Self {
        Self { file: None, console_ok: false, path: PathBuf::new() }
    }

    pub fn line(&mut self, s: &str) {
        self.file_only(s);
        self.console(s);
    }

    /// The console half of [`Tee::line`]: best effort, given up for good
    /// after the first failure.
    pub fn console(&mut self, s: &str) {
        if self.console_ok && writeln!(io::stdout().lock(), "{s}").is_err() {
            self.console_ok = false;
        }
    }

    pub fn flush_file(&mut self) {
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }
    }

    /// Detail that would flood the console but belongs in the record.
    pub fn file_only(&mut self, s: &str) {
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{s}");
        }
    }

    pub fn flush(&mut self) {
        self.flush_file();
        if self.console_ok {
            let _ = io::stdout().flush();
        }
    }
}

enum TeeMsg {
    Line(String),
    FileOnly(String),
}

/// A [`Tee`] on its own thread. Sending never blocks, so a thread that must
/// stay responsive (the key logger's input loop) never waits on the console
/// or the disk. The file is flushed whenever the writer catches up.
pub struct TeeThread {
    tx: Option<Sender<TeeMsg>>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
}

impl TeeThread {
    pub fn spawn(mut tee: Tee) -> Self {
        let path = tee.path.clone();
        let (tx, rx) = mpsc::channel::<TeeMsg>();
        let thread = std::thread::Builder::new()
            .name("log-writer".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // Take everything that is queued. Write and flush it to
                    // the file first, then show it: a console whose output is
                    // frozen (Pause key, a text selection) can hold up the
                    // console write indefinitely, but never the file.
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    for m in &batch {
                        match m {
                            TeeMsg::Line(s) | TeeMsg::FileOnly(s) => tee.file_only(s),
                        }
                    }
                    tee.flush_file();
                    for m in &batch {
                        if let TeeMsg::Line(s) = m {
                            tee.console(s);
                        }
                    }
                    tee.flush();
                }
                tee.flush();
            })
            .ok();
        Self { tx: Some(tx), thread, path }
    }

    pub fn line(&self, s: impl Into<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TeeMsg::Line(s.into()));
        }
    }

    pub fn file_only(&self, s: impl Into<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TeeMsg::FileOnly(s.into()));
        }
    }

    /// Stop accepting lines and wait until everything sent is written.
    pub fn close(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TeeThread {
    fn drop(&mut self) {
        self.close();
    }
}

/// Wall-clock time for log lines, to the millisecond.
pub fn now_stamp() -> String {
    stamp(chrono::Local::now())
}

pub fn stamp(t: chrono::DateTime<chrono::Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}
