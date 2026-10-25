//! Turning per-key colours into 0x8081 (PER_KEY_LIGHTING_V2) requests.
//!
//! Keys of one colour go four or more at a time in fn6 (one colour, up to 13
//! zones); the rest four to a request in fn1 (`[zone, r, g, b]` each). A list
//! shorter than a request holds ends in `FF`. Nothing shows until the frame
//! end (fn7), which is always sent with persistence 0: the frame lives in the
//! keyboard's RAM only, and the keyboard's own effect draws over it again.

use std::collections::BTreeMap;

use crate::lighting::Rgb;

pub const SET_ZONES: u8 = 1;
pub const SET_ONE_COLOUR: u8 = 6;
pub const FRAME_END: u8 = 7;

/// fn7 with persistence 0 (RAM only), frame 0, no next frame. Persistence 1
/// is never sent from here.
pub const FRAME_END_RAM: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x00];

/// The requests, as (function, parameters), that paint `keys` (zone,
/// colour), not including the frame end.
pub fn requests(keys: &[(u8, Rgb)]) -> Vec<(u8, Vec<u8>)> {
    let mut by_colour: BTreeMap<Rgb, Vec<u8>> = BTreeMap::new();
    for &(zone, c) in keys {
        by_colour.entry(c).or_default().push(zone);
    }
    let mut out = Vec::new();
    let mut singles: Vec<(u8, Rgb)> = Vec::new();
    for (c, zones) in by_colour {
        let mut rest = zones.as_slice();
        while rest.len() >= 4 {
            let (chunk, tail) = rest.split_at(rest.len().min(13));
            let mut p = c.0.to_vec();
            p.extend_from_slice(chunk);
            if chunk.len() < 13 {
                p.push(0xFF);
            }
            out.push((SET_ONE_COLOUR, p));
            rest = tail;
        }
        singles.extend(rest.iter().map(|&z| (z, c)));
    }
    for chunk in singles.chunks(4) {
        let mut p = Vec::with_capacity(16);
        for (z, c) in chunk {
            p.push(*z);
            p.extend_from_slice(&c.0);
        }
        if chunk.len() < 4 {
            p.push(0xFF);
        }
        out.push((SET_ZONES, p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = Rgb([0xFF, 0, 0]);
    const BLUE: Rgb = Rgb([0, 0, 0xFF]);

    #[test]
    fn one_key_is_one_zone_request_ended_by_ff() {
        assert_eq!(requests(&[(0x26, RED)]), [(SET_ZONES, vec![0x26, 0xFF, 0, 0, 0xFF])]);
    }

    #[test]
    fn many_keys_of_one_colour_share_requests() {
        let keys: Vec<(u8, Rgb)> = (0..30).map(|z| (z, BLUE)).collect();
        let r = requests(&keys);
        // 13 + 13 in fn6, and the last 4 in a third fn6.
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|(f, _)| *f == SET_ONE_COLOUR));
        assert_eq!(r[0].1.len(), 16);
        assert_eq!(r[2].1, [0, 0, 0xFF, 26, 27, 28, 29, 0xFF]);
    }

    #[test]
    fn leftovers_of_several_colours_fill_zone_requests() {
        let keys = [(1, RED), (2, RED), (3, BLUE), (4, BLUE), (5, BLUE)];
        let r = requests(&keys);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|(f, p)| *f == SET_ZONES && p.len() <= 16));
        let zones: Vec<u8> = r
            .iter()
            .flat_map(|(_, p)| p.chunks(4).filter(|c| c.len() == 4).map(|c| c[0]))
            .collect();
        let mut zones = zones;
        zones.sort();
        assert_eq!(zones, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn every_request_fits_a_long_report() {
        let keys: Vec<(u8, Rgb)> = (0..116).map(|z| (z, Rgb([z, 0, 0]))).collect();
        assert!(requests(&keys).iter().all(|(_, p)| p.len() <= 16));
    }

    #[test]
    fn the_frame_end_is_ram_only() {
        assert_eq!(FRAME_END_RAM[0], 0);
    }
}
