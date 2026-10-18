//! The `cursor.highlight` buttons: while one is on, an arrow as big as Windows' stands
//! in for the arrow cursor, so the cursor is easy to find over a busy screen. The arrow
//! is drawn after Blue Archive's own cursor, in the colour chosen for the button on the
//! panel, and handed to Windows as a cursor file (`.cur`).
//!
//! The game's cursor (read off Windows on 2026-10-18, 32 by 32, with its alpha) is a
//! dot-wide navy edge, a white rim inside it and a fill whose hue turns from cyan at the
//! left to violet at the right point, its tip cut short by a three-dot top, and a pale
//! see-through shadow under it. Its edges are smoothed in alpha. The shape, the shadow
//! and the colours here were fitted to it.

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

/// The arrow's outline on a 32 by 32 cursor, from its tip: a three-dot top cutting
/// the point short, the slanting side, the right point, the notch under it, and the
/// left side.
const ARROW: [(f32, f32); 9] = [
    (0.1, 1.5),
    (1.2, 0.1),
    (4.0, 0.1),
    (17.45, 12.3),
    (22.55, 16.9),
    (23.45, 20.6),
    (11.7, 23.1),
    (3.5, 29.3),
    (0.1, 28.6),
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
/// How far the shadow falls right and down of the arrow, and how much of what is
/// under it shows through it.
const SHADOW: (f32, f32) = (0.2, 1.8);
const SHADOW_ALPHA: f32 = 0.33;
/// How wide the navy edge is, in the cursor's own pixels at any size: a dot, with the
/// next one shading off as the game's (scaled with the arrow, a bigger cursor's edge
/// spread over two dots, the user's of 2026-10-18: 「左2列目の黒…薄くする」).
const EDGE_PIXELS: f32 = 1.25;
/// The inner corner of the tip, shaded a little grey so the tip reads as even on both
/// sides (the user's of 2026-10-18: 「左上から3ドット・3ドットの座標を暗くする」): its
/// centre and reach on a 32 by 32 cursor, how far toward `CORNER` it goes and that grey.
const CORNER_AT: (f32, f32) = (2.0, 2.0);
const CORNER_REACH: f32 = 0.75;
const CORNER_SHADE: f32 = 0.75;
const CORNER: [f32; 3] = [174.0, 181.0, 191.0];
/// The edge's, the shadow's and the rim's colours.
const NAVY: [f32; 3] = [47.0, 62.0, 89.0];
const SHADE: [f32; 3] = [55.0, 70.0, 95.0];
const WHITE: [f32; 3] = [248.0, 251.0, 255.0];
/// Samples a pixel is drawn from, across and down: enough to shade an edge as finely
/// as the game's.
const SAMPLES: u32 = 8;

/// The point of the arrow, where the cursor points, on a cursor `size` pixels wide:
/// the point of the outline nearest the corner its short top cuts off, a dot inside
/// the corner (the game's cursor points at that corner, a dot off its tip; this one
/// points at the tip).
pub fn hotspot(size: u32) -> (u16, u16) {
    let mut tip = ARROW[0];
    let mut nearest = f32::INFINITY;
    for corner in 0..ARROW.len() {
        let (from, to) = (ARROW[corner], ARROW[(corner + 1) % ARROW.len()]);
        let (run, rise) = (to.0 - from.0, to.1 - from.1);
        let along =
            ((-from.0 * run - from.1 * rise) / run.mul_add(run, rise * rise)).clamp(0.0, 1.0);
        let point = (along.mul_add(run, from.0), along.mul_add(rise, from.1));
        let far = point.0.hypot(point.1);
        if far < nearest {
            (tip, nearest) = (point, far);
        }
    }
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
/// straight (not premultiplied) RGBA.
pub fn picture(size: u32, palette: Palette) -> Vec<[u8; 4]> {
    (0..size * size)
        .map(|at| pixel(size, (at % size, at / size), palette))
        .collect()
}

/// One pixel: its samples' colours mixed by how much of each covers it (the arrow
/// whole, its shadow at `SHADOW_ALPHA`).
fn pixel(size: u32, (column, row): (u32, u32), palette: Palette) -> [u8; 4] {
    let unit = 32.0 / size as f32;
    let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
    for sample in 0..SAMPLES * SAMPLES {
        let across = (column as f32 + ((sample % SAMPLES) as f32 + 0.5) / SAMPLES as f32) * unit;
        let down = (row as f32 + ((sample / SAMPLES) as f32 + 0.5) / SAMPLES as f32) * unit;
        if let Some((colour, alpha)) = colour(across, down, palette, EDGE_PIXELS * unit) {
            weight += alpha;
            for channel in 0..3 {
                sum[channel] = colour[channel].mul_add(alpha, sum[channel]);
            }
        }
    }
    if weight <= 0.0 {
        return [0; 4];
    }
    let byte = |value: f32| u8::try_from(to_byte_range(value, 255.0)).unwrap_or(u8::MAX);
    let [r, g, b] = sum.map(|channel| byte(channel / weight));
    [r, g, b, byte(weight / (SAMPLES * SAMPLES) as f32 * 255.0)]
}

/// The colour at (`across`, `down`), on a 32 by 32 cursor's units, and how opaque it
/// is: the navy edge (`edge` wide, in those units), the white rim, the fill, or the
/// shadow; `None` off them all.
fn colour(across: f32, down: f32, palette: Palette, edge: f32) -> Option<([f32; 3], f32)> {
    let depth = -distance(&ARROW, across, down);
    if depth > 0.0 {
        if depth < edge {
            return Some((NAVY, 1.0));
        }
        if distance(&FILL, across, down) < 0.0 {
            return Some((fill(across, down, palette), 1.0));
        }
        // The rim: white, tinged with the fill's second colour toward the right point.
        let tinge =
            0.6 * ((across - 9.0) / 6.0).clamp(0.0, 1.0) * ((down - 12.0) / 4.0).clamp(0.0, 1.0);
        let pale = hsv(palette.hues().1, 0.12);
        let rim = [0, 1, 2].map(|i| (pale[i] - WHITE[i]).mul_add(tinge, WHITE[i]));
        let near = (across - CORNER_AT.0).hypot(down - CORNER_AT.1) / CORNER_REACH;
        let shade = CORNER_SHADE * (1.0 - near).max(0.0);
        return Some((
            [0, 1, 2].map(|i| (CORNER[i] - rim[i]).mul_add(shade, rim[i])),
            1.0,
        ));
    }
    (distance(&ARROW, across - SHADOW.0, down - SHADOW.1) < 0.0).then_some((SHADE, SHADOW_ALPHA))
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

    /// The point is cut short by a short top, and the cursor points at the tip, not at
    /// the corner cut off (the user's of 2026-10-18: the game's points a dot off).
    #[test]
    fn the_tip_is_cut_short_and_the_cursor_points_at_it() {
        let size = 64;
        let picture = picture(size, Palette::Blue);
        let alpha = |(column, row): (u16, u16)| {
            picture[usize::from(row) * usize::try_from(size).unwrap() + usize::from(column)][3]
        };
        assert_eq!(alpha((0, 0)), 0, "the corner is cut off");
        assert!(alpha(hotspot(size)) > 128, "the cursor points at the arrow");
        assert_eq!(hotspot(32), (1, 1));
    }

    /// The slanting side is one dot of navy wide all the way down, as the game's (the
    /// user's of 2026-10-18: 「1ドット分の太さを保っています」).
    #[test]
    fn the_slanting_side_stays_a_dot_wide() {
        let size = 32;
        let picture = picture(size, Palette::Blue);
        for row in 3..=12 {
            let dark = (4..size)
                .map(|column| picture[usize::try_from(row * size + column).unwrap()])
                .filter(|p| p[3] >= 0xc0 && p[..3].iter().all(|&c| c < 110))
                .count();
            assert_eq!(dark, 1, "row {row}");
        }
    }

    /// On a 40-pixel cursor (125% scaling) as on the game's 32: the left edge's first
    /// column is dark and the second a light grey, not two dark ones.
    #[test]
    fn the_left_edge_is_a_dark_dot_then_a_light_one_at_any_size() {
        for size in [32, 40, 48] {
            let picture = picture(size, Palette::Blue);
            let row = usize::try_from(size / 2 * size).unwrap();
            let (first, second) = (picture[row], picture[row + 1]);
            assert!(
                first[3] >= 0xd0 && first[..3].iter().all(|&c| c < 110),
                "{size}: {first:?}"
            );
            assert!(
                second[..3].iter().all(|&c| (150..=200).contains(&c)),
                "{size}: {second:?}"
            );
        }
    }

    /// The inner corner of the tip is shaded, so the tip reads as even: on a 40-pixel
    /// cursor the third dot down and across is greyer than the rim beyond it.
    #[test]
    fn the_tips_inner_corner_is_shaded() {
        let size = 40;
        let picture = picture(size, Palette::Blue);
        let at = |(column, row): (usize, usize)| picture[row * 40 + column];
        let lightness = |p: [u8; 4]| p[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        assert!(
            lightness(at((2, 2))) + 40 < lightness(at((3, 4))),
            "{:?}",
            at((2, 2))
        );
    }

    /// From the left edge in: the navy edge, the white rim, then the fill.
    #[test]
    fn a_navy_edge_and_a_white_rim_frame_the_fill() {
        let size = 32;
        let edge = at(size, Palette::Blue, (0.4, 14.0));
        assert!(edge[..3].iter().all(|&c| c < 110), "{edge:?}");
        let rim = at(size, Palette::Blue, (2.6, 14.0));
        assert!(rim[..3].iter().all(|&c| c > 230), "{rim:?}");
        let fill = at(size, Palette::Blue, (8.0, 14.0));
        assert!(fill[0] < 230 && fill[2] > 240, "{fill:?}");
    }

    /// Under the right wing a pale shadow lets what is under it show through (the
    /// user's of 2026-10-18: the game's is a thin grey, not black).
    #[test]
    fn a_pale_shadow_falls_under_the_right_wing() {
        let shadow = at(64, Palette::Green, (18.0, 22.8));
        assert!((40..=130).contains(&shadow[3]), "{shadow:?}");
        assert!(shadow[..3].iter().all(|&c| c < 110), "{shadow:?}");
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
