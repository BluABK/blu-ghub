//! Pasting for the G-keys: text, or a file (an image also as image data).
//!
//! The clipboard's contents are saved, the paste goes on it, Ctrl+V is
//! pressed, and after a moment for the program to read it the saved
//! contents go back, unless something else was copied meanwhile. What is
//! put there is marked to stay out of Windows' clipboard history and cloud
//! clipboard. Formats held as memory blocks are saved and restored (text,
//! files, rich text, images as DIB); GDI handles (CF_BITMAP and the
//! metafiles) are not, and Windows makes CF_BITMAP from the DIB again.
//!
//! A file goes on as a file (CF_HDROP, as Explorer's Copy puts it, so chat
//! programs upload it and a GIF stays animated); an image file also as a
//! bitmap (CF_DIB, decoded by Windows Imaging Component) and, for a PNG,
//! its own bytes ("PNG", which browsers and chat programs take with
//! transparency).

use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{GENERIC_READ, GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::core::{HSTRING, PCWSTR, w};

type Result<T> = std::result::Result<T, String>;

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// How long the program pasted into gets to read the clipboard before what
/// was there goes back.
const READ_TIME: Duration = Duration::from_millis(400);

/// Paste `text`.
pub fn paste_text(text: &str) -> Result<()> {
    paste(vec![(CF_UNICODETEXT, unicode_text(text))])
}

/// Paste the file at `path`; an image also as image data.
pub fn paste_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(format!("no file at {}", path.display()));
    }
    let mut items = vec![(CF_HDROP, drop_files(path))];
    // Explorer's "copy, not move" for the file.
    let effect = register(w!("Preferred DropEffect"));
    items.push((effect, 1u32.to_le_bytes().to_vec()));
    if is_image(path) {
        match dib(path) {
            Ok(d) => items.push((CF_DIB, d)),
            Err(e) => tracing::debug!("paste: {} as an image: {e}", path.display()),
        }
        if has_extension(path, &["png"])
            && let Ok(bytes) = std::fs::read(path)
        {
            items.push((register(w!("PNG")), bytes));
        }
    }
    paste(items)
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

pub fn is_image(path: &Path) -> bool {
    has_extension(path, &["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "ico"])
}

fn register(name: PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

fn paste(items: Vec<(u32, Vec<u8>)>) -> Result<()> {
    let saved = {
        let _open = Open::new()?;
        let saved = save();
        unsafe { EmptyClipboard() }.map_err(|e| format!("emptying the clipboard: {e}"))?;
        for (format, bytes) in &items {
            put(*format, bytes)?;
        }
        keep_out_of_history();
        saved
    };
    let ours = unsafe { GetClipboardSequenceNumber() };
    crate::inject::ctrl_v().map_err(|e| format!("Ctrl+V: {e}"))?;
    std::thread::sleep(READ_TIME);
    // Put back what was there, unless something was copied meanwhile.
    if unsafe { GetClipboardSequenceNumber() } == ours {
        let _open = Open::new()?;
        unsafe { EmptyClipboard() }.map_err(|e| format!("emptying the clipboard: {e}"))?;
        for (format, bytes) in &saved {
            if let Err(e) = put(*format, bytes) {
                tracing::debug!("paste: restoring format {format}: {e}");
            }
        }
        // Already in the history once; not again.
        if !saved.is_empty() {
            keep_out_of_history();
        }
    }
    Ok(())
}

/// Marks the clipboard's contents for Windows' clipboard history, cloud
/// clipboard and clipboard monitors to leave alone.
fn keep_out_of_history() {
    let no = 0u32.to_le_bytes();
    let _ = put(register(w!("ExcludeClipboardContentFromMonitorProcessing")), &no);
    let _ = put(register(w!("CanIncludeInClipboardHistory")), &no);
    let _ = put(register(w!("CanUploadToCloudClipboard")), &no);
}

/// The clipboard, open; closed when dropped. Another program may hold it
/// for a moment, so opening is tried a few times.
struct Open;

impl Open {
    fn new() -> Result<Open> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("the clipboard is held by another program".into())
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Formats held as plain memory blocks, which can be copied and put back:
/// not the GDI ones (bitmap, metafile, palette, enhanced metafile), the
/// owner-drawn ones or the private and GDI-object ranges.
fn is_memory_format(f: u32) -> bool {
    !matches!(f, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E) && !(0x200..=0x3FF).contains(&f)
}

/// What the clipboard holds, as (format, bytes). The clipboard must be
/// open.
fn save() -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut f = 0;
    loop {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        if !is_memory_format(f) {
            continue;
        }
        let Ok(h) = (unsafe { GetClipboardData(f) }) else { continue };
        let mem = HGLOBAL(h.0);
        unsafe {
            let size = GlobalSize(mem);
            let p = GlobalLock(mem) as *const u8;
            if p.is_null() {
                continue;
            }
            out.push((f, std::slice::from_raw_parts(p, size).to_vec()));
            let _ = GlobalUnlock(mem);
        }
    }
    out
}

/// Put `bytes` on the (open) clipboard as `format`.
fn put(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
            .map_err(|e| format!("memory for the clipboard: {e}"))?;
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err("memory for the clipboard".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(mem);
        // The clipboard owns the memory once this succeeds.
        if let Err(e) = SetClipboardData(format, Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(format!("putting format {format} on the clipboard: {e}"));
        }
    }
    Ok(())
}

/// NUL-terminated UTF-16, as CF_UNICODETEXT holds it.
fn unicode_text(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// A DROPFILES block naming one file: the 20-byte header (offset of the
/// list, a drop point, two flags: not client area, wide characters), then
/// the path in UTF-16 and two NULs.
fn drop_files(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(20u32.to_le_bytes()); // pFiles
    out.extend([0u8; 8]); // pt
    out.extend(0u32.to_le_bytes()); // fNC
    out.extend(1u32.to_le_bytes()); // fWide
    let wide = path.as_os_str().to_string_lossy();
    out.extend(wide.encode_utf16().chain([0, 0]).flat_map(u16::to_le_bytes));
    out
}

/// A packed DIB (BITMAPINFOHEADER and pixels) from 32-bit BGRA rows, top
/// row first; the DIB holds them bottom row first.
fn dib_from_bgra(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(40 + pixels.len());
    out.extend(40u32.to_le_bytes()); // biSize
    out.extend((width as i32).to_le_bytes());
    out.extend((height as i32).to_le_bytes()); // positive: bottom-up
    out.extend(1u16.to_le_bytes()); // biPlanes
    out.extend(32u16.to_le_bytes()); // biBitCount
    out.extend(0u32.to_le_bytes()); // BI_RGB
    out.extend((pixels.len() as u32).to_le_bytes()); // biSizeImage
    out.extend([0u8; 16]); // resolution, colours used, important
    for row in pixels.chunks(stride).rev() {
        out.extend_from_slice(row);
    }
    out
}

/// The image file decoded (its first frame) into a DIB. COM must be
/// initialised on this thread.
fn dib(path: &Path) -> Result<Vec<u8>> {
    let e = |what: &str, err: windows::core::Error| format!("{what}: {err}");
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|x| e("image decoder", x))?;
        let decoder = factory
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .map_err(|x| e("reading the image", x))?;
        let frame = decoder.GetFrame(0).map_err(|x| e("the image's first frame", x))?;
        let converter = factory.CreateFormatConverter().map_err(|x| e("converter", x))?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(|x| e("converting the image", x))?;
        let (mut w, mut h) = (0u32, 0u32);
        converter.GetSize(&mut w, &mut h).map_err(|x| e("image size", x))?;
        let size = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4));
        let Some(size) = size.filter(|s| *s > 0 && *s < u32::MAX as usize) else {
            return Err(format!("an image of {w} x {h} is too big"));
        };
        let mut pixels = vec![0u8; size];
        converter
            .CopyPixels(std::ptr::null(), w * 4, &mut pixels)
            .map_err(|x| e("the image's pixels", x))?;
        Ok(dib_from_bgra(w, h, &pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_nul_terminated_utf16() {
        assert_eq!(unicode_text("Hé"), [b'H', 0, 0xE9, 0, 0, 0]);
    }

    #[test]
    fn a_dropped_file_has_the_header_and_two_nuls() {
        let d = drop_files(Path::new(r"C:\x.png"));
        assert_eq!(&d[..4], &20u32.to_le_bytes());
        assert_eq!(&d[16..20], &1u32.to_le_bytes());
        let tail: Vec<u16> = d[20..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16_lossy(&tail[..8]), r"C:\x.png");
        assert_eq!(&tail[8..], &[0, 0]);
    }

    #[test]
    fn a_dib_is_stored_bottom_row_first() {
        // 1 x 2: a red row over a blue one (BGRA).
        let pixels = [0, 0, 255, 255, 255, 0, 0, 255];
        let d = dib_from_bgra(1, 2, &pixels);
        assert_eq!(d.len(), 40 + 8);
        assert_eq!(&d[8..12], &2i32.to_le_bytes());
        assert_eq!(&d[40..44], &[255, 0, 0, 255]); // blue first
    }

    #[test]
    fn gdi_formats_are_not_copied() {
        assert!(is_memory_format(CF_UNICODETEXT));
        assert!(is_memory_format(CF_DIB));
        assert!(!is_memory_format(2)); // CF_BITMAP
        assert!(!is_memory_format(14)); // CF_ENHMETAFILE
        assert!(!is_memory_format(0x300));
        assert!(is_memory_format(0xC0FF)); // a registered one
    }

    #[test]
    fn images_are_told_by_their_extension() {
        assert!(is_image(Path::new("a.PNG")));
        assert!(is_image(Path::new("b.gif")));
        assert!(!is_image(Path::new("c.txt")));
    }
}
