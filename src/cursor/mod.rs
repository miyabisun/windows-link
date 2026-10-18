//! The `cursor.highlight` buttons: while one is on, an arrow half again as big as
//! Windows' stands in for the arrow cursor, so the cursor is easy to find over a busy
//! screen. The arrow is drawn after Blue Archive's own cursor (a triangle in a dark
//! edge and a white rim, filled with a gradient), in the button's colour, and handed
//! to Windows as a cursor file (`.cur`).

#![allow(
    clippy::cast_precision_loss,
    reason = "a cursor is at most 256 pixels square: its numbers fit f32 exactly"
)]

use serde::Deserialize;

pub mod windows;

/// The colour a `cursor.highlight` button fills the arrow with: blue as the game's
/// own, red and green to stand out where the screen is blue (2026-10-18).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Palette {
    Red,
    Green,
    #[default]
    Blue,
}

impl Palette {
    /// The fill's colours at the arrow's left edge and at its right point.
    fn fill(self) -> [[f32; 3]; 2] {
        let rgb = |c: [u8; 3]| c.map(|v| f32::from(v) / 255.0);
        match self {
            // As the game's cursor draws it: cyan to violet.
            Self::Blue => [rgb([140, 232, 255]), rgb([214, 172, 255])],
            Self::Red => [rgb([255, 120, 120]), rgb([255, 150, 210])],
            Self::Green => [rgb([110, 240, 150]), rgb([210, 250, 120])],
        }
    }

    /// Its name, for the cursor file's.
    pub fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }
}

/// The arrow's corners on a 32 by 32 cursor, its point (the hotspot) first: Blue
/// Archive's cursor, a triangle cut short at its top and with a notch under its
/// right point.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (5.0, 0.0),
    (23.3, 16.6),
    (23.3, 22.6),
    (12.0, 25.4),
    (5.2, 31.0),
    (0.0, 31.0),
];
/// The arrow's right end, where the fill reaches its second colour.
const RIGHT: f32 = 23.3;
/// How wide the dark edge and the white rim inside it are, on a 32 by 32 cursor.
const EDGE: f32 = 1.6;
const RIM: f32 = 2.0;
/// The dark edge's colour.
const DARK: [f32; 3] = [49.0 / 255.0, 61.0 / 255.0, 83.0 / 255.0];

/// The point of the arrow, where the cursor points, on a cursor `size` pixels wide.
pub fn hotspot(size: u32) -> (u16, u16) {
    let at = |unit: f32| {
        u16::try_from(to_byte_range(
            unit * size as f32 / 32.0,
            f32::from(u16::MAX),
        ))
        .unwrap_or(0)
    };
    (at(ARROW[0].0), at(ARROW[0].1))
}

/// `value` rounded into 0..=`most`, for a pixel's place or colour (a cursor is at most
/// 256 pixels square, so its numbers are small).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to a small non-negative range first"
)]
fn to_byte_range(value: f32, most: f32) -> u32 {
    value.round().clamp(0.0, most) as u32
}

/// The arrow in `palette`, `size` by `size` pixels, row by row from the top, as
/// straight (not premultiplied) RGBA.
pub fn picture(size: u32, palette: Palette) -> Vec<[u8; 4]> {
    (0..size * size)
        .map(|at| pixel(size, (at % size, at / size), palette))
        .collect()
}

/// One pixel: the dark edge, the white rim inside it, then the fill inside that.
fn pixel(size: u32, (column, row): (u32, u32), palette: Palette) -> [u8; 4] {
    let unit = 32.0 / size as f32;
    let across = (column as f32 + 0.5) * unit;
    let down = (row as f32 + 0.5) * unit;
    // How far outside the arrow, in pixels: negative inside it.
    let outside = distance(across, down) / unit;
    let pixels = |units: f32| units / unit;
    let covered = |inset: f32| (0.5 - outside - pixels(inset)).clamp(0.0, 1.0);
    let mut out = [0.0f32; 4];
    over(&mut out, DARK, covered(0.0));
    over(&mut out, [1.0; 3], covered(EDGE));
    over(&mut out, fill(across, down, palette), covered(EDGE + RIM));
    out.map(|channel| u8::try_from(to_byte_range(channel * 255.0, 255.0)).unwrap_or(u8::MAX))
}

/// The fill at (`across`, `down`): the palette's left colour to its right one from
/// the left edge to the right point, paler toward the bottom, as the game's.
fn fill(across: f32, down: f32, palette: Palette) -> [f32; 3] {
    let [left, right] = palette.fill();
    let t = (across / RIGHT).clamp(0.0, 1.0);
    let pale = ((down - 14.0) / 10.0).clamp(0.0, 1.0) * 0.55;
    [0, 1, 2].map(|i| {
        let base = (right[i] - left[i]).mul_add(t, left[i]);
        (1.0 - base).mul_add(pale, base)
    })
}

/// Paints `color` at `alpha` over `under` (straight alpha).
fn over(under: &mut [f32; 4], color: [f32; 3], alpha: f32) {
    let total = alpha + under[3] * (1.0 - alpha);
    if total <= 0.0 {
        return;
    }
    for channel in 0..3 {
        under[channel] =
            (color[channel] * alpha + under[channel] * under[3] * (1.0 - alpha)) / total;
    }
    under[3] = total;
}

/// How far (`across`, `down`) lies from the arrow's outline, negative inside it.
fn distance(across: f32, down: f32) -> f32 {
    let mut nearest = f32::INFINITY;
    let mut inside = false;
    for corner in 0..ARROW.len() {
        let (from, to) = (ARROW[corner], ARROW[(corner + 1) % ARROW.len()]);
        let (run, rise) = (to.0 - from.0, to.1 - from.1);
        let along = (((across - from.0) * run + (down - from.1) * rise)
            / run.mul_add(run, rise * rise))
        .clamp(0.0, 1.0);
        let gap = (along.mul_add(run, from.0) - across).hypot(along.mul_add(rise, from.1) - down);
        nearest = nearest.min(gap);
        if (from.1 > down) != (to.1 > down) && across < (down - from.1).mul_add(run / rise, from.0)
        {
            inside = !inside;
        }
    }
    if inside { -nearest } else { nearest }
}

/// A cursor file of `picture` (see `picture`), `size` by `size`, pointing at
/// `hotspot`: a 32-bit picture with its alpha, and an empty mask.
pub fn cur(picture: &[[u8; 4]], size: u32, hotspot: (u16, u16)) -> Vec<u8> {
    let mask_row = size.div_ceil(32) * 4;
    let image = size * size * 4 + mask_row * size;
    // A width of 256 is written 0.
    let width_byte = u8::try_from(size).unwrap_or(0);
    let mut out = Vec::new();
    for value in [0u16, 2, 1] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&[width_byte, width_byte, 0, 0]);
    out.extend_from_slice(&hotspot.0.to_le_bytes());
    out.extend_from_slice(&hotspot.1.to_le_bytes());
    out.extend_from_slice(&(40 + image).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    for value in [40u32, size, size * 2] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, image, 0, 0, 0, 0] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    // Rows from the bottom, blue first.
    let width = usize::try_from(size).unwrap_or(0);
    for row in picture.chunks(width).rev() {
        for [red, green, blue, alpha] in row {
            out.extend_from_slice(&[*blue, *green, *red, *alpha]);
        }
    }
    out.extend(std::iter::repeat_n(
        0u8,
        usize::try_from(mask_row * size).unwrap_or(0),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel at (`across`, `down`) on a 32 by 32 cursor's units, drawn `size` wide.
    fn at(size: u32, palette: Palette, (across, down): (f32, f32)) -> [u8; 4] {
        let scale = size as f32 / 32.0;
        let (column, row) = (
            to_byte_range(across * scale, f32::from(u16::MAX)),
            to_byte_range(down * scale, f32::from(u16::MAX)),
        );
        picture(size, palette)[usize::try_from(row * size + column).unwrap()]
    }

    /// The arrow is solid inside and clear beyond its outline: nothing right of its
    /// slanting top, nothing in the far corner.
    #[test]
    fn the_arrow_is_solid_inside_and_clear_beyond_its_edge() {
        let size = 48;
        assert_eq!(at(size, Palette::Blue, (6.0, 14.0))[3], 255);
        assert_eq!(at(size, Palette::Blue, (20.0, 6.0))[3], 0);
        assert_eq!(at(size, Palette::Blue, (31.0, 1.0))[3], 0);
        assert_eq!(at(size, Palette::Blue, (20.0, 29.0))[3], 0);
    }

    /// From the left edge in: the dark edge, the white rim, then the fill.
    #[test]
    fn a_dark_edge_and_a_white_rim_frame_the_fill() {
        let size = 96;
        let edge = at(size, Palette::Blue, (0.4, 14.0));
        assert!(edge[..3].iter().all(|&c| c < 110), "{edge:?}");
        let rim = at(size, Palette::Blue, (2.6, 14.0));
        assert!(rim[..3].iter().all(|&c| c > 230), "{rim:?}");
        let fill = at(size, Palette::Blue, (8.0, 14.0));
        assert!(fill[0] < 230 && fill[2] > 240, "{fill:?}");
    }

    /// Each palette fills the arrow with its own colour.
    #[test]
    fn each_palette_fills_its_colour() {
        let fill = |palette| at(64, palette, (8.0, 12.0));
        let [r, g, b, _] = fill(Palette::Red);
        assert!(r > g + 60 && r > b + 40, "red {:?}", [r, g, b]);
        let [r, g, b, _] = fill(Palette::Green);
        assert!(g > r + 40 && g > b + 30, "green {:?}", [r, g, b]);
        let [r, g, b, _] = fill(Palette::Blue);
        assert!(b > r + 40 && b > g, "blue {:?}", [r, g, b]);
    }

    /// Windows takes the file for a cursor of the size asked.
    #[test]
    fn windows_loads_the_cursor_file_at_its_size() {
        let size = 48;
        let path =
            std::env::temp_dir().join(format!("windows-link-cursor-{}.cur", std::process::id()));
        std::fs::write(
            &path,
            cur(&picture(size, Palette::Red), size, hotspot(size)),
        )
        .unwrap();
        let loaded = windows::loaded_size(&path, size);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, Some(size));
    }
}
