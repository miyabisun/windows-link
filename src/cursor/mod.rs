//! The `cursor.highlight` buttons: while one is on and its program is in front, a
//! rainbow arrow half again as big as Windows' stands in for the arrow cursor, so the
//! cursor is easy to find over a busy game. The picture is drawn here and handed to
//! Windows as an animated cursor file (`.ani`).

#![allow(
    clippy::cast_precision_loss,
    reason = "a cursor is at most 256 pixels square: its numbers fit f32 exactly"
)]

pub mod windows;

/// Frames of the rainbow, and how long each shows in 1/60 s: a turn a second.
pub const FRAMES: usize = 12;
pub const JIFFIES: u32 = 5;

/// The arrow's corners on a 32 by 32 cursor, its tip first; room around it for the
/// dark edge and the glow.
const ARROW: [(f32, f32); 7] = [
    (4.0, 4.0),
    (4.0, 25.0),
    (9.0, 20.0),
    (13.0, 29.0),
    (16.0, 28.0),
    (12.0, 19.0),
    (19.0, 19.0),
];
/// How wide the dark edge and the glow outside it are, on a 32 by 32 cursor.
const EDGE: f32 = 1.2;
const GLOW: f32 = 3.0;

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

/// The frames of the rainbow arrow, `size` by `size` pixels each, row by row from the
/// top, as straight (not premultiplied) RGBA.
pub fn frames(size: u32) -> Vec<Vec<[u8; 4]>> {
    (0..FRAMES)
        .map(|frame| {
            let phase = frame as f32 / FRAMES as f32;
            (0..size * size)
                .map(|at| pixel(size, (at % size, at / size), phase))
                .collect()
        })
        .collect()
}

/// One pixel: the glow, then the dark edge, then the rainbow fill over them.
fn pixel(size: u32, (column, row): (u32, u32), phase: f32) -> [u8; 4] {
    let unit = 32.0 / size as f32;
    let across = (column as f32 + 0.5) * unit;
    let down = (row as f32 + 0.5) * unit;
    // How far outside the arrow, in pixels: negative inside it.
    let outside = distance(across, down) / unit;
    let pixels = |units: f32| units / unit;
    let hue = (phase + (across + down) / 64.0).fract();
    let rainbow = hsv(hue, 0.8, 1.0);
    let mut out = [0.0f32; 4];
    let glow = (1.0 - (outside - pixels(EDGE)) / pixels(GLOW)).clamp(0.0, 1.0) * 0.6;
    over(&mut out, rainbow, glow);
    let edge = (pixels(EDGE) + 0.5 - outside).clamp(0.0, 1.0);
    over(&mut out, [0.12, 0.12, 0.12], edge);
    over(&mut out, rainbow, (0.5 - outside).clamp(0.0, 1.0));
    out.map(|channel| u8::try_from(to_byte_range(channel * 255.0, 255.0)).unwrap_or(u8::MAX))
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

/// The colour of `hue` (0 to 1 round the wheel) at `saturation` and `value`.
fn hsv(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let sixths = hue * 6.0;
    let within = sixths.fract();
    let low = value * (1.0 - saturation);
    let falling = value * saturation.mul_add(-within, 1.0);
    let rising = value * saturation.mul_add(within - 1.0, 1.0);
    match to_byte_range(sixths.floor(), 5.0) {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    }
}

/// An animated cursor file of `frames` (see `frames`), each `size` by `size`,
/// pointing at `hotspot`, each shown `JIFFIES`.
pub fn ani(frames: &[Vec<[u8; 4]>], size: u32, hotspot: (u16, u16)) -> Vec<u8> {
    let count = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    let mut header = Vec::new();
    for value in [36, count, count, 0, 0, 0, 0, JIFFIES, 1] {
        header.extend_from_slice(&u32::to_le_bytes(value));
    }
    let mut list = b"fram".to_vec();
    for frame in frames {
        list.extend(chunk(*b"icon", &cur(frame, size, hotspot)));
    }
    let mut body = b"ACON".to_vec();
    body.extend(chunk(*b"anih", &header));
    body.extend(chunk(*b"LIST", &list));
    chunk(*b"RIFF", &body)
}

fn chunk(id: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// One frame as a `.cur` file: a 32-bit picture with its alpha, and an empty mask.
fn cur(frame: &[[u8; 4]], size: u32, hotspot: (u16, u16)) -> Vec<u8> {
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
    for row in frame.chunks(width).rev() {
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

    fn at(frame: &[[u8; 4]], size: u32, column: u32, row: u32) -> [u8; 4] {
        frame[usize::try_from(row * size + column).unwrap()]
    }

    /// The arrow is solid from its tip down its body, clear in the far corner, and its
    /// colours move from frame to frame.
    #[test]
    fn the_arrow_is_solid_at_its_tip_and_its_rainbow_turns() {
        let size = 48;
        let frames = frames(size);
        assert_eq!(frames.len(), FRAMES);
        let (hx, hy) = hotspot(size);
        let inside = at(&frames[0], size, u32::from(hx) + 2, u32::from(hy) + 8);
        assert_eq!(inside[3], 255);
        assert_eq!(at(&frames[0], size, size - 1, 0)[3], 0);
        let later = at(
            &frames[FRAMES / 2],
            size,
            u32::from(hx) + 2,
            u32::from(hy) + 8,
        );
        assert_ne!(inside[..3], later[..3]);
    }

    /// Windows takes the file for a cursor of the size asked.
    #[test]
    fn windows_loads_the_cursor_file_at_its_size() {
        let size = 48;
        let path =
            std::env::temp_dir().join(format!("windows-link-cursor-{}.ani", std::process::id()));
        std::fs::write(&path, ani(&frames(size), size, hotspot(size))).unwrap();
        let loaded = windows::loaded_size(&path, size);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, Some(size));
    }
}
