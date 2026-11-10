//! Pictures of the user's own, drawn into cursors: `cursor-<name>.png` files in the data
//! folder (the user's of 2026-11-10: a halo turning while a program is busy, one to aim
//! with over a link). None are shipped; the repository holds no artwork.

use std::io::Cursor;

/// Samples a pixel is drawn from, across and down.
const SAMPLES: u32 = 4;
/// How see-through a dot is still glow, which gets no edge, and from where it is solid
/// enough for a full one.
const GLOW: f32 = 0.25;
const SOLID: f32 = 0.6;

/// A picture: rows from the top, straight (not premultiplied) RGBA.
pub struct Art {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

impl Art {
    /// A picture of `pixels`, `width` wide (`None` if they do not fill its rows).
    pub fn new(width: u32, pixels: Vec<[u8; 4]>) -> Option<Self> {
        let count = u32::try_from(pixels.len()).ok()?;
        (width > 0 && count > 0 && count.is_multiple_of(width)).then(|| Self {
            width,
            height: count / width,
            pixels,
        })
    }

    /// The picture in the PNG `file`, whatever its colour type.
    pub fn decode(file: &[u8]) -> Result<Self, String> {
        let mut decoder = png::Decoder::new(Cursor::new(file));
        decoder.set_transformations(
            png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
        );
        let mut reader = decoder.read_info().map_err(|err| err.to_string())?;
        let mut data = vec![0; reader.output_buffer_size().ok_or("too big a picture")?];
        let frame = reader
            .next_frame(&mut data)
            .map_err(|err| err.to_string())?;
        let data = &data[..frame.buffer_size()];
        let pixels = match frame.color_type {
            png::ColorType::Rgba => data
                .chunks_exact(4)
                .map(|p| [p[0], p[1], p[2], p[3]])
                .collect(),
            png::ColorType::GrayscaleAlpha => data
                .chunks_exact(2)
                .map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            other => return Err(format!("a {other:?} picture")),
        };
        Self::new(frame.width, pixels).ok_or_else(|| "an empty picture".to_owned())
    }

    /// The picture on a clear ground `side` pixels square: its longer side `across`
    /// pixels, its middle at `centre`, turned `turn` degrees clockwise about it. A navy
    /// edge is drawn round it, a dot wide and the next shading off as the arrow's, so a
    /// pale picture shows over a pale ground.
    pub fn drawn(&self, side: u32, centre: (f32, f32), across: f32, turn: f32) -> Vec<[u8; 4]> {
        let laid = self.laid(side, centre, across, turn);
        let wide = i64::from(side);
        // How solid the picture is at a dot, a glow not at all.
        let solid = |column: i64, row: i64| {
            if column < 0 || row < 0 || column >= wide || row >= wide {
                return 0.0;
            }
            let dot = laid[usize::try_from(row * wide + column).unwrap_or(0)];
            ((f32::from(dot[3]) / 255.0 - GLOW) / (SOLID - GLOW)).clamp(0.0, 1.0)
        };
        let byte = |value: f32| u8::try_from(super::to_byte_range(value, 255.0)).unwrap_or(u8::MAX);
        (0..side * side)
            .map(|at| {
                let (column, row) = (i64::from(at % side), i64::from(at / side));
                let mut edge = 0.0f32;
                for (down, right) in
                    (-2i16..=2).flat_map(|down| (-2i16..=2).map(move |r| (down, r)))
                {
                    let near = (2.0 - f32::from(right).hypot(f32::from(down))).clamp(0.0, 1.0);
                    let there = solid(column + i64::from(right), row + i64::from(down));
                    edge = edge.max(near * there);
                }
                let [r, g, b, a] = laid[usize::try_from(at).unwrap_or(0)];
                let over = f32::from(a) / 255.0;
                let under = edge * (1.0 - over);
                if under <= 0.0 {
                    return [r, g, b, a];
                }
                let mix = |channel: u8, navy: f32| {
                    byte(f32::from(channel).mul_add(over, navy * under) / (over + under))
                };
                let [nr, ng, nb] = super::NAVY;
                [
                    mix(r, nr),
                    mix(g, ng),
                    mix(b, nb),
                    byte((over + under) * 255.0),
                ]
            })
            .collect()
    }

    /// As `drawn`, before its edge.
    fn laid(&self, side: u32, centre: (f32, f32), across: f32, turn: f32) -> Vec<[u8; 4]> {
        let scale = self.width.max(self.height) as f32 / across;
        let (sin, cos) = turn.to_radians().sin_cos();
        let (wide, high) = (self.width as f32, self.height as f32);
        let byte = |value: f32| u8::try_from(super::to_byte_range(value, 255.0)).unwrap_or(u8::MAX);
        (0..side * side)
            .map(|at| {
                // Colours weighed by their alpha, and the alpha itself.
                let mut sum = [0.0f32; 4];
                for sample in 0..SAMPLES * SAMPLES {
                    let part = |step: u32| (step as f32 + 0.5) / SAMPLES as f32;
                    let right = (at % side) as f32 + part(sample % SAMPLES) - centre.0;
                    let down = (at / side) as f32 + part(sample / SAMPLES) - centre.1;
                    // Back through the turn, onto the picture's own dots.
                    let column = right
                        .mul_add(cos, down * sin)
                        .mul_add(scale, wide / 2.0)
                        .floor();
                    let row = down
                        .mul_add(cos, -right * sin)
                        .mul_add(scale, high / 2.0)
                        .floor();
                    if column < 0.0 || column >= wide || row < 0.0 || row >= high {
                        continue;
                    }
                    let dot = super::to_byte_range(row, high) * self.width
                        + super::to_byte_range(column, wide);
                    let [r, g, b, a] = self.pixels[usize::try_from(dot).unwrap_or(0)];
                    let alpha = f32::from(a);
                    for (total, channel) in sum.iter_mut().zip([r, g, b]) {
                        *total = f32::from(channel).mul_add(alpha, *total);
                    }
                    sum[3] += alpha;
                }
                if sum[3] <= 0.0 {
                    return [0; 4];
                }
                let [r, g, b] = [0, 1, 2].map(|channel| byte(sum[channel] / sum[3]));
                [r, g, b, byte(sum[3] / (SAMPLES * SAMPLES) as f32)]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    /// Four dots square, its left half red and its right half blue.
    fn halves() -> Art {
        Art::new(4, [RED, RED, BLUE, BLUE].repeat(4)).unwrap()
    }

    fn at(picture: &[[u8; 4]], side: usize, (column, row): (usize, usize)) -> [u8; 4] {
        picture[row * side + column]
    }

    /// The picture is drawn as wide as asked around the point asked, and nothing beyond.
    #[test]
    fn a_picture_is_drawn_at_the_size_and_place_asked() {
        let drawn = halves().laid(16, (8.0, 8.0), 8.0, 0.0);
        assert_eq!(drawn.len(), 256);
        assert_eq!(at(&drawn, 16, (5, 8)), RED);
        assert_eq!(at(&drawn, 16, (10, 8)), BLUE);
        assert_eq!(at(&drawn, 16, (2, 8))[3], 0);
        assert_eq!(at(&drawn, 16, (8, 13))[3], 0);
    }

    /// Turned a quarter clockwise, what was at the left is at the top.
    #[test]
    fn a_turned_picture_carries_its_colours_round() {
        let drawn = halves().laid(16, (8.0, 8.0), 8.0, 90.0);
        assert_eq!(at(&drawn, 16, (8, 5)), RED);
        assert_eq!(at(&drawn, 16, (8, 10)), BLUE);
    }

    /// A dot half covered is half see-through, in the picture's colour.
    #[test]
    fn an_edge_is_smoothed_in_alpha() {
        let drawn = halves().laid(16, (8.5, 8.0), 8.0, 0.0);
        let edge = at(&drawn, 16, (4, 8));
        assert_eq!(edge[..3], RED[..3]);
        assert!((100..=155).contains(&edge[3]), "{edge:?}");
    }

    /// A pale picture shows over a pale ground by a navy edge round it, as the arrow
    /// has: the dots next to it are navy, the picture itself is as it was, and a faint
    /// glow gets no edge.
    #[test]
    fn a_navy_edge_is_drawn_round_the_picture_but_not_round_a_glow() {
        let drawn = halves().drawn(16, (8.0, 8.0), 8.0, 0.0);
        assert_eq!(at(&drawn, 16, (5, 8)), RED);
        let edge = at(&drawn, 16, (3, 8));
        assert_eq!(edge[3], 255);
        assert!(edge[..3].iter().all(|&c| c < 110), "{edge:?}");
        assert_eq!(at(&drawn, 16, (1, 8))[3], 0);

        let glow = Art::new(1, vec![[255, 255, 255, 40]]).unwrap();
        let drawn = glow.drawn(16, (8.0, 8.0), 4.0, 0.0);
        assert_eq!(at(&drawn, 16, (7, 7)), [255, 255, 255, 40]);
        assert_eq!(at(&drawn, 16, (5, 7))[3], 0);
    }

    /// A PNG without alpha is read as solid, one with it keeps it.
    #[test]
    fn a_png_is_read_with_or_without_alpha() {
        let with = crate::icons::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 64]).unwrap();
        let art = Art::decode(&with).unwrap();
        assert_eq!((art.width, art.height), (2, 1));
        assert_eq!(art.pixels, [RED, [0, 0, 255, 64]]);

        let mut without = Vec::new();
        let mut encoder = png::Encoder::new(&mut without, 1, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 0, 255, 255, 0, 0]).unwrap();
        writer.finish().unwrap();
        let art = Art::decode(&without).unwrap();
        assert_eq!((art.width, art.height), (1, 2));
        assert_eq!(art.pixels, [BLUE, RED]);
        assert!(Art::decode(b"not a picture").is_err());
    }
}
