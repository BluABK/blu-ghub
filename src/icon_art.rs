// The app icon as pixels, drawn from a description rather than shipped as a
// file, and with **no dependencies**, because `build.rs` includes this same
// source.
//
// Windows takes the icon for a shortcut, an Explorer listing and the taskbar
// out of the executable's own resources; the icon a running app gives its
// tray entry is a separate thing. Drawing both from this one function keeps
// them from drifting apart: the build script renders it into the `.ico` it
// embeds, the app renders it for the tray.
//
// The design: a dark keycap on an amber tile — unlike TunMan's cyan portal
// and StreamArchiver's purple tile, which share the same tray. Telling them
// apart at 16 px matters more than detail.

/// Sizes embedded in the `.ico`. Windows picks per context (16 for menus, 32
/// for the taskbar, 48 and 256 for Explorer); a missing size is scaled from
/// another, which smears at small sizes. Read by `build.rs`.
#[allow(dead_code)]
pub const SIZES: &[u32] = &[16, 20, 24, 32, 48, 64, 128, 256];

const AMBER: [u8; 3] = [0xf5, 0xa6, 0x23];
/// The keycap's skirt.
const CAP: [u8; 3] = [0x1c, 0x21, 0x2b];
/// Its top face, a shade lighter so the cap reads as a key, not a hole.
const FACE: [u8; 3] = [0x3a, 0x42, 0x52];

/// Inside a rounded rectangle?
fn in_rounded(fx: f32, fy: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if fx < x0 || fx > x1 || fy < y0 || fy > y1 {
        return false;
    }
    let cx = fx.clamp(x0 + r, x1 - r);
    let cy = fy.clamp(y0 + r, y1 - r);
    (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r
}

/// The whole design in a 32×32 coordinate space. `None` is transparent.
fn sample(fx: f32, fy: f32) -> Option<[u8; 3]> {
    if !in_rounded(fx, fy, 0.0, 0.0, 32.0, 32.0, 5.0) {
        return None;
    }
    if in_rounded(fx, fy, 10.0, 8.5, 22.0, 20.0, 2.0) {
        return Some(FACE);
    }
    if in_rounded(fx, fy, 7.0, 7.0, 25.0, 25.0, 3.0) {
        return Some(CAP);
    }
    Some(AMBER)
}

/// The icon at `size`, as RGBA8, supersampled so the small sizes keep their
/// shape.
pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let scale = size as f32 / 32.0;
    let n = (SS * SS) as f32;
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut covered) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) / scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) / scale;
                    if let Some(c) = sample(fx, fy) {
                        r += c[0] as f32;
                        g += c[1] as f32;
                        b += c[2] as f32;
                        covered += 1.0;
                    }
                }
            }
            if covered == 0.0 {
                continue;
            }
            let i = ((y * size + x) * 4) as usize;
            // Colour averages the samples that hit; alpha counts them.
            out[i] = (r / covered).round() as u8;
            out[i + 1] = (g / covered).round() as u8;
            out[i + 2] = (b / covered).round() as u8;
            out[i + 3] = (255.0 * covered / n).round() as u8;
        }
    }
    out
}

/// The icon as a Windows `.ico` holding every size in `sizes`.
#[allow(dead_code)]
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|s| dib(*s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, data) in sizes.iter().zip(&images) {
        // 256 is written as 0: the field is one byte.
        let dim = if *s >= 256 { 0u8 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for data in images {
        out.extend_from_slice(&data);
    }
    out
}

/// One icon image as a 32-bit DIB: header, bottom-up BGRA, then the AND mask
/// (structurally required, left "opaque": alpha decides transparency). This
/// is also the format `CreateIconFromResourceEx` takes, which is how the app
/// makes its tray icon.
pub fn dib(size: u32) -> Vec<u8> {
    let px = rgba(size);
    let and_len = size.div_ceil(32) * 4 * size;
    let xor_len = size * size * 4;
    let mut v = Vec::with_capacity(40 + (xor_len + and_len) as usize);
    v.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER size
    v.extend_from_slice(&(size as i32).to_le_bytes());
    // Height counts the XOR and AND masks together.
    v.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // planes
    v.extend_from_slice(&32u16.to_le_bytes()); // bpp
    v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    v.extend_from_slice(&(xor_len + and_len).to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            v.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]);
        }
    }
    v.resize(v.len() + and_len as usize, 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_the_size_that_was_asked_for() {
        for s in [16u32, 32, 256] {
            assert_eq!(rgba(s).len(), (s * s * 4) as usize, "at {s}px");
        }
    }

    /// The keycap has to survive 16 px, the size the tray uses.
    #[test]
    fn the_keycap_is_still_visible_at_16px() {
        let px = rgba(16);
        let at = |x: u32, y: u32| {
            let i = ((y * 16 + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert!(at(8, 7)[0] < 0x60, "centre should be dark: {:?}", at(8, 7));
        assert!(at(1, 8)[0] > 0xc0, "edge should be amber: {:?}", at(1, 8));
        assert!(at(0, 0)[3] < 0x60, "corner should be mostly transparent: {:?}", at(0, 0));
    }

    #[test]
    fn the_ico_container_is_well_formed() {
        let sizes = [16u32, 32, 256];
        let v = ico(&sizes);
        assert_eq!(&v[0..2], &[0, 0], "reserved");
        assert_eq!(&v[2..4], &[1, 0], "type 1 = icon");
        assert_eq!(&v[4..6], &[3, 0], "three images");
        let mut expected_offset = 6 + 16 * sizes.len() as u32;
        for (i, s) in sizes.iter().enumerate() {
            let e = 6 + 16 * i;
            let len = u32::from_le_bytes([v[e + 8], v[e + 9], v[e + 10], v[e + 11]]);
            let off = u32::from_le_bytes([v[e + 12], v[e + 13], v[e + 14], v[e + 15]]);
            assert_eq!(off, expected_offset, "offset for {s}");
            assert_eq!(len, 40 + s * s * 4 + s.div_ceil(32) * 4 * s, "byte length for {s}");
            expected_offset += len;
        }
        assert_eq!(v.len() as u32, expected_offset, "trailing or missing bytes");
    }
}
