//! Shared terminal palette defaults.
//!
//! Rendering owns theme selection, but VT OSC color queries need stable fallback
//! RGB values without depending on the renderer crate.

use crate::Rgb;

pub const DEFAULT_FG: Rgb = Rgb::new(0xe0, 0xe0, 0xe0);
pub const DEFAULT_BG: Rgb = Rgb::new(0x1e, 0x1e, 0x1e);
pub const DEFAULT_CURSOR: Rgb = DEFAULT_FG;

/// Return the standard 256-color xterm palette.
pub fn xterm_palette() -> [Rgb; 256] {
    let mut p = [Rgb::new(0, 0, 0); 256];

    const ANSI16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    for (i, &(r, g, b)) in ANSI16.iter().enumerate() {
        p[i] = Rgb::new(r, g, b);
    }

    const STEP: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let mut idx = 16usize;
    for &r in &STEP {
        for &g in &STEP {
            for &b in &STEP {
                p[idx] = Rgb::new(r, g, b);
                idx += 1;
            }
        }
    }

    for i in 0..24u8 {
        let v = 8 + i * 10;
        p[232 + i as usize] = Rgb::new(v, v, v);
    }

    p
}

pub fn xterm_palette_color(index: u8) -> Rgb {
    xterm_palette()[index as usize]
}
