//! The `cursor.highlight` buttons: while one is on, an arrow as big as Windows' stands
//! in for the arrow cursor, so the cursor is easy to find over a busy screen. The arrow
//! is drawn after Blue Archive's own cursor, in the colour chosen for the button on the
//! panel, and handed to Windows as a cursor file (`.cur`).
//!
//! The game's cursor (read off Windows on 2026-10-18, 32 by 32) is lit from the top
//! left: a dark navy edge, a white rim inside it and a fill whose hue turns from cyan
//! at the left to violet at the right point, with an opaque shadow a little below and
//! right of it. Every pixel is opaque or clear; the edges are smoothed in colour, not
//! in alpha. The shape, the shadow and the colours here were fitted to it.

#![allow(
    clippy::cast_precision_loss,
    reason = "a cursor is at most 256 pixels square: its numbers fit f32 exactly"
)]

use serde::{Deserialize, Serialize};

pub mod windows;

/// The colour a `cursor.highlight` button fills the arrow with, chosen on the panel:
/// blue as the game's own, red and green to stand out where the screen is blue
/// (2026-10-18).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Palette {
    Red,
    Green,
    #[default]
    Blue,
}

impl Palette {
    /// The fill's hues, in degrees, at the arrow's left edge and at its right point,
    /// where the next colour mixes in: blue takes red (cyan to violet, as the game's
    /// cursor), red takes green (to amber) and green takes blue (to teal), the user's
    /// of 2026-10-18.
    fn hues(self) -> (f32, f32) {
        match self {
            Self::Blue => (191.0, 269.0),
            Self::Red => (350.0, 410.0),
            Self::Green => (130.0, 190.0),
        }
    }

    /// Its name, as the API and the database write it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }

    /// The palette of `name` (see `name`).
    pub fn named(name: &str) -> Option<Self> {
        [Self::Red, Self::Green, Self::Blue]
            .into_iter()
            .find(|p| p.name() == name)
    }
}

/// The arrow's corners on a 32 by 32 cursor, its point (the hotspot's corner) first:
/// a triangle cut short at its top, with a notch under its right point.
const ARROW: [(f32, f32); 8] = [
    (0.0, 0.0),
    (4.86, 0.0),
    (17.62, 12.3),
    (22.75, 16.57),
    (22.75, 21.05),
    (12.49, 23.1),
    (3.67, 29.27),
    (0.12, 29.22),
];
/// Where the fill is, inside the white rim: the rim is wider along the slanting top
/// and under the right point than along the left edge.
const FILL: [(f32, f32); 7] = [
    (4.0, 6.0),
    (5.0, 6.0),
    (17.0, 17.6),
    (14.0, 18.6),
    (10.0, 19.6),
    (7.0, 21.2),
    (4.0, 24.8),
];
/// How far the shadow falls right and down of the arrow.
const SHADOW: (f32, f32) = (0.5, 1.8);
/// How wide the dark edge is.
const EDGE: f32 = 1.42;
/// The dark edge's, the shadow's and the rim's colours.
const NAVY: [f32; 3] = [49.0, 61.0, 83.0];
const SHADE: [f32; 3] = [26.0, 29.0, 38.0];
const WHITE: [f32; 3] = [248.0, 251.0, 255.0];
/// Samples a pixel is drawn from, across and down.
const SAMPLES: u32 = 4;

/// How round the tip is: the radius its two corners are rounded off at, and how many
/// points draw each rounded corner.
const TIP: f32 = 2.0;
const ARC: usize = 8;

/// The arrow's outline: `ARROW` with the tip's two corners rounded off at `TIP`.
fn outline() -> Vec<(f32, f32)> {
    let mut points = Vec::new();
    for (i, &corner) in ARROW.iter().enumerate() {
        if i > 1 {
            points.push(corner);
            continue;
        }
        let toward = |to: (f32, f32)| {
            let (run, rise) = (to.0 - corner.0, to.1 - corner.1);
            let length = run.hypot(rise);
            (run / length, rise / length)
        };
        let (back, ahead) = (
            toward(ARROW[(i + ARROW.len() - 1) % ARROW.len()]),
            toward(ARROW[i + 1]),
        );
        // Half the corner's angle, and the circle touching both its sides.
        let half = back
            .0
            .mul_add(ahead.0, back.1 * ahead.1)
            .clamp(-1.0, 1.0)
            .acos()
            / 2.0;
        let middle = (back.0 + ahead.0, back.1 + ahead.1);
        let reach = TIP / half.sin() / middle.0.hypot(middle.1);
        let centre = (
            middle.0.mul_add(reach, corner.0),
            middle.1.mul_add(reach, corner.1),
        );
        let along = TIP / half.tan();
        let angle = |at: (f32, f32)| {
            let point = (
                back.0.mul_add(at.0, corner.0),
                back.1.mul_add(at.0, corner.1),
            );
            let point = (
                ahead.0.mul_add(at.1, point.0),
                ahead.1.mul_add(at.1, point.1),
            );
            (point.1 - centre.1).atan2(point.0 - centre.0)
        };
        let (from, to) = (angle((along, 0.0)), angle((0.0, along)));
        // The short way round, from the side behind to the side ahead.
        let mut sweep = to - from;
        if sweep > std::f32::consts::PI {
            sweep -= std::f32::consts::TAU;
        } else if sweep < -std::f32::consts::PI {
            sweep += std::f32::consts::TAU;
        }
        for k in 0..=ARC {
            let at = (k as f32 / ARC as f32).mul_add(sweep, from);
            points.push((
                TIP.mul_add(at.cos(), centre.0),
                TIP.mul_add(at.sin(), centre.1),
            ));
        }
    }
    points
}

/// The point of the arrow, where the cursor points, on a cursor `size` pixels wide:
/// the round tip's point nearest the corner it rounds off, a dot or so inside it (the
/// game's cursor points a dot off its round tip; this one points at it).
pub fn hotspot(size: u32) -> (u16, u16) {
    let corner = ARROW[0];
    let tip = outline()
        .into_iter()
        .min_by(|a, b| {
            let far = |p: &(f32, f32)| (p.0 - corner.0).hypot(p.1 - corner.1);
            far(a).total_cmp(&far(b))
        })
        .unwrap_or(corner);
    let at = |unit: f32| {
        u16::try_from(to_byte_range(
            unit * size as f32 / 32.0,
            f32::from(u16::MAX),
        ))
        .unwrap_or(0)
    };
    (at(tip.0), at(tip.1))
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
/// straight (not premultiplied) RGBA: each pixel opaque or clear.
pub fn picture(size: u32, palette: Palette) -> Vec<[u8; 4]> {
    let outline = outline();
    (0..size * size)
        .map(|at| pixel(size, (at % size, at / size), palette, &outline))
        .collect()
}

/// One pixel: the colours of its samples that fall on the arrow or its shadow, mixed;
/// opaque when they cover half of it or more, clear otherwise.
fn pixel(
    size: u32,
    (column, row): (u32, u32),
    palette: Palette,
    outline: &[(f32, f32)],
) -> [u8; 4] {
    let unit = 32.0 / size as f32;
    let (mut sum, mut covered) = ([0.0f32; 3], 0);
    for sample in 0..SAMPLES * SAMPLES {
        let across = (column as f32 + ((sample % SAMPLES) as f32 + 0.5) / SAMPLES as f32) * unit;
        let down = (row as f32 + ((sample / SAMPLES) as f32 + 0.5) / SAMPLES as f32) * unit;
        if let Some(colour) = colour(outline, across, down, palette) {
            covered += 1;
            for channel in 0..3 {
                sum[channel] += colour[channel];
            }
        }
    }
    if covered * 2 < SAMPLES * SAMPLES {
        return [0; 4];
    }
    let [r, g, b] = sum.map(|channel| {
        u8::try_from(to_byte_range(channel / covered as f32, 255.0)).unwrap_or(u8::MAX)
    });
    [r, g, b, u8::MAX]
}

/// The colour at (`across`, `down`), on a 32 by 32 cursor's units: the dark edge, the
/// white rim, the fill, or the shadow; `None` off the arrow and its shadow.
fn colour(outline: &[(f32, f32)], across: f32, down: f32, palette: Palette) -> Option<[f32; 3]> {
    let depth = -distance(outline, across, down);
    if depth > 0.0 {
        if depth < EDGE {
            return Some(NAVY);
        }
        if distance(&FILL, across, down) < 0.0 {
            return Some(fill(across, down, palette));
        }
        // The rim: white, tinged with the fill's second colour toward the right point.
        let tinge =
            0.6 * ((across - 9.0) / 6.0).clamp(0.0, 1.0) * ((down - 12.0) / 4.0).clamp(0.0, 1.0);
        let pale = hsv(palette.hues().1, 0.12);
        return Some([0, 1, 2].map(|i| (pale[i] - WHITE[i]).mul_add(tinge, WHITE[i])));
    }
    (distance(outline, across - SHADOW.0, down - SHADOW.1) < 0.0).then_some(SHADE)
}

/// The fill at (`across`, `down`), as fitted to the game's: its hue turns from the
/// palette's first to its second toward the right point; it is deepest high at the
/// left, paler down and across, and deep again in a streak just short of the point.
fn fill(across: f32, down: f32, palette: Palette) -> [f32; 3] {
    let (first, second) = palette.hues();
    let turned = ((across - 4.0) / 12.0).clamp(0.0, 1.0).powf(2.2);
    let hue = (second - first).mul_add(turned, first);
    let left = (0.48 - (down - 8.0) * 0.021).max(0.12);
    let across_it = ((across - 4.0) / 8.0).clamp(0.0, 1.0);
    let streak = 0.2 * (-((across - 16.3) / 1.2).powi(2) - ((down - 17.1) / 0.9).powi(2)).exp();
    let saturation = left.mul_add(1.0 - across_it, 0.18 * across_it) + streak;
    hsv(hue, saturation)
}

/// The colour of `hue` (degrees, any turn) at `saturation` and full value, 0 to 255.
fn hsv(hue: f32, saturation: f32) -> [f32; 3] {
    let sixths = hue.rem_euclid(360.0) / 60.0;
    let within = sixths.fract();
    let (full, low) = (255.0, 255.0 * (1.0 - saturation));
    let falling = 255.0 * saturation.mul_add(-within, 1.0);
    let rising = 255.0 * saturation.mul_add(within - 1.0, 1.0);
    match to_byte_range(sixths.floor(), 5.0) {
        0 => [full, rising, low],
        1 => [falling, full, low],
        2 => [low, full, rising],
        3 => [low, falling, full],
        4 => [rising, low, full],
        _ => [full, low, falling],
    }
}

/// How far (`across`, `down`) lies from `outline`, negative inside it.
fn distance(outline: &[(f32, f32)], across: f32, down: f32) -> f32 {
    let mut nearest = f32::INFINITY;
    let mut inside = false;
    for corner in 0..outline.len() {
        let (from, to) = (outline[corner], outline[(corner + 1) % outline.len()]);
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

    /// The arrow is solid inside and clear beyond its outline and its shadow: nothing
    /// right of its slanting top, nothing in the far corners.
    #[test]
    fn the_arrow_is_solid_inside_and_clear_beyond_its_edge() {
        let size = 48;
        assert_eq!(at(size, Palette::Blue, (6.0, 14.0))[3], 255);
        assert_eq!(at(size, Palette::Blue, (20.0, 6.0))[3], 0);
        assert_eq!(at(size, Palette::Blue, (31.0, 1.0))[3], 0);
        assert_eq!(at(size, Palette::Blue, (20.0, 29.0))[3], 0);
    }

    /// As the game's, each pixel is opaque or clear: the edges are smoothed in colour.
    #[test]
    fn every_pixel_is_opaque_or_clear() {
        for size in [32, 40, 48] {
            assert!(
                picture(size, Palette::Red)
                    .iter()
                    .all(|p| p[3] == 0 || p[3] == 255)
            );
        }
    }

    /// The tip is round, and the cursor points at the round tip, not at the corner
    /// it rounds off (the user's of 2026-10-18: the game's tip is round and a dot off).
    #[test]
    fn the_tip_is_round_and_the_cursor_points_at_it() {
        let size = 64;
        let picture = picture(size, Palette::Blue);
        let alpha = |(column, row): (u16, u16)| {
            picture[usize::from(row) * usize::try_from(size).unwrap() + usize::from(column)][3]
        };
        assert_eq!(alpha((0, 0)), 0, "the corner is rounded off");
        let tip = hotspot(size);
        assert_eq!(tip, (1, 1));
        assert!(alpha(tip) > 128, "the cursor points at the arrow");
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

    /// A dark shadow falls under the arrow's right wing, as the light comes from the
    /// top left.
    #[test]
    fn a_shadow_falls_under_the_right_wing() {
        let shadow = at(64, Palette::Green, (18.0, 23.2));
        assert_eq!(shadow[3], 255);
        assert!(shadow[..3].iter().all(|&c| c < 60), "{shadow:?}");
    }

    /// Each palette fills the arrow with its own colour.
    #[test]
    fn each_palette_fills_its_colour() {
        let fill = |palette| at(64, palette, (6.0, 12.0));
        let [r, g, b, _] = fill(Palette::Red);
        assert!(r > g + 60 && r > b + 40, "red {:?}", [r, g, b]);
        let [r, g, b, _] = fill(Palette::Green);
        assert!(g > r + 40 && g > b + 30, "green {:?}", [r, g, b]);
        let [r, g, b, _] = fill(Palette::Blue);
        assert!(b > r + 40 && b > g, "blue {:?}", [r, g, b]);
    }

    /// Toward its right point each palette mixes in the next colour, as the game's
    /// blue turns violet: blue takes red, red takes green, green takes blue (the
    /// user's of 2026-10-18).
    #[test]
    fn toward_its_right_point_each_palette_mixes_in_the_next_colour() {
        let point = |palette| at(64, palette, (15.5, 17.0));
        let [r, g, _, _] = point(Palette::Blue);
        assert!(r > g, "blue takes red {:?}", point(Palette::Blue));
        let [_, g, b, _] = point(Palette::Red);
        assert!(g > b + 30, "red takes green {:?}", point(Palette::Red));
        let [r, _, b, _] = point(Palette::Green);
        assert!(b > r + 60, "green takes blue {:?}", point(Palette::Green));
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
