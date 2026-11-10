//! Halos drawn into cursors (the user's of 2026-11-10): Blue Archive students' halos,
//! drawn here from their shapes as measured off pictures of them (350 pixels square),
//! not copied from the pictures. Anna's is in the busy cursors, three to aim with stand
//! in for Windows' cross (over a link they hid the link's words), and Junko's, narrow
//! and pointing with its top, for the hand over a link.

use std::sync::LazyLock;

use super::Palette;

/// A halo, after the student who wears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halo {
    /// A gear: a ring with fourteen teeth, and in it a small ring with six teeth round
    /// a six-sided one. Purple at the top turns light blue at the bottom, and the
    /// colours go round it, the gear still, while a program is busy.
    Anna,
    /// A sight: two rings, and four marks across the outer one. Light blue.
    Shiroko,
    /// Two thin rings and four stars across them, each with a long point inward. Green.
    Midori,
    /// Midori's shape in pink.
    Momoi,
    /// Taller than wide: a long four-sided point up, a dot under it, and an arc at each
    /// side of the dot with a barb at each end. Her own is red; here in the colour given.
    Junko(Palette),
}

impl Halo {
    /// The halo to aim with while the highlight is in `palette`.
    pub fn aim(palette: Palette) -> Self {
        match palette {
            Palette::Blue => Self::Shiroko,
            Palette::Green => Self::Midori,
            Palette::Red => Self::Momoi,
        }
    }

    /// The halo on a clear ground `side` pixels square, `across` pixels wide with its
    /// middle at `centre`, once for each of `turns`: how many degrees clockwise its
    /// colours have gone round (its shape stays). A navy edge is drawn round it, as
    /// round the arrow, and its thinnest lines are kept wide enough to show.
    pub fn drawn(
        self,
        side: u32,
        centre: (f32, f32),
        across: f32,
        turns: &[f32],
    ) -> Vec<Vec<[u8; 4]>> {
        let (reach, thin) = self.measures();
        // The edge lies outside the halo's lines, so the lines stop short of `across`.
        let radius = across / 2.0 - super::EDGE_PIXELS - 0.5;
        // The picture's pixels to one of the cursor's dots.
        let scale = reach / radius;
        let edge = super::EDGE_PIXELS * scale;
        let grow = ((THINNEST - thin / scale) / 2.0).max(0.0) * scale;
        let place = |at: u32, part: (f32, f32)| {
            (
                (at % side) as f32 + part.0 - centre.0,
                (at / side) as f32 + part.1 - centre.1,
            )
        };
        // How much of each dot the halo's lines cover, and how much their edge.
        let cover: Vec<(f32, f32)> = (0..side * side)
            .map(|at| {
                let (mut lines, mut rim) = (0.0f32, 0.0f32);
                for sample in 0..SAMPLES * SAMPLES {
                    let part = |step: u32| (step as f32 + 0.5) / SAMPLES as f32;
                    let (x, y) = place(at, (part(sample % SAMPLES), part(sample / SAMPLES)));
                    let depth = self.depth(x * scale, y * scale) + grow;
                    if depth > 0.0 {
                        lines += 1.0;
                    } else if depth > -edge {
                        rim += 1.0;
                    }
                }
                let samples = (SAMPLES * SAMPLES) as f32;
                (lines / samples, rim / samples)
            })
            .collect();
        let byte = |value: f32| u8::try_from(super::to_byte_range(value, 255.0)).unwrap_or(u8::MAX);
        turns
            .iter()
            .map(|&turn| {
                cover
                    .iter()
                    .zip(0u32..)
                    .map(|(&(lines, rim), at)| {
                        if lines + rim <= 0.0 {
                            return [0; 4];
                        }
                        let (right, down) = place(at, (0.5, 0.5));
                        let own = self.colour(right / radius, down / radius, turn);
                        let [r, g, b] = [0, 1, 2].map(|i| {
                            byte(own[i].mul_add(lines, super::NAVY[i] * rim) / (lines + rim))
                        });
                        [r, g, b, byte((lines + rim) * 255.0)]
                    })
                    .collect()
            })
            .collect()
    }

    /// How far the halo reaches from its middle and how wide its thinnest line is, in
    /// its picture's pixels.
    fn measures(self) -> (f32, f32) {
        match self {
            Self::Anna => (141.25, 6.6),
            Self::Shiroko => (153.5, 15.0),
            Self::Midori | Self::Momoi => (151.5, 7.35),
            Self::Junko(_) => (159.5, 9.5),
        }
    }

    /// How far inside the halo's lines (`x`, `y`) is, in its picture's pixels from its
    /// middle; negative outside them.
    fn depth(self, x: f32, y: f32) -> f32 {
        let far = x.hypot(y);
        let ring = |inner: f32, outer: f32| (far - inner).min(outer - far);
        match self {
            Self::Anna => {
                let teeth = bar(spoke(x, y, 14.0), (110.0, 141.25), (10.0, 6.0));
                let small = bar(spoke(x, y, 6.0), (45.0, 57.25), (7.5, 5.5));
                // Six sides, a corner up: how far out the side (`x`, `y`) lies on is.
                let six = x.abs().max(0.5f32.mul_add(x.abs(), 0.866 * y.abs()));
                let nut = (34.0 - six).min(six - 27.4);
                ring(96.25, 115.5)
                    .max(teeth)
                    .max(ring(39.0, 48.0))
                    .max(small)
                    .max(nut)
            }
            Self::Shiroko => {
                let marks = bar(spoke(x, y, 4.0), (88.0, 153.5), (9.0, 9.0));
                ring(48.0, 63.0).max(ring(104.75, 128.75)).max(marks)
            }
            Self::Midori | Self::Momoi => {
                let (along, aside) = spoke(x, y, 4.0);
                let stars = -super::distance(&STAR, aside, along);
                ring(95.4, 102.75).max(ring(122.3, 131.2)).max(stars)
            }
            Self::Junko(_) => {
                // The same at both sides of the middle; the arcs' and the dot's middle is
                // below the halo's own.
                let aside = x.abs();
                let round = aside.hypot(y - 49.5);
                let arc = (round - 68.3)
                    .min(77.8 - round)
                    .min(y + 19.5)
                    .min(aside - 10.5);
                let dot = 18.0 - aside.hypot(y - 51.0);
                let barbs =
                    [&UPPER_BARB[..], &LOWER_BARB[..]].map(|barb| -super::distance(barb, aside, y));
                (-super::distance(&POINT, x, y))
                    .max(dot)
                    .max(arc)
                    .max(barbs[0])
                    .max(barbs[1])
            }
        }
    }

    /// The halo's colour at (`x`, `y`), its reach being 1 from its middle, its colours
    /// gone `turn` degrees round: each shades from one colour at the top to another at
    /// the bottom, and only Anna's go round.
    fn colour(self, x: f32, y: f32, turn: f32) -> [f32; 3] {
        let shade = |top: [f32; 3], bottom: [f32; 3], part: f32| {
            [0, 1, 2].map(|i| (bottom[i] - top[i]).mul_add(part.clamp(0.0, 1.0), top[i]))
        };
        match self {
            Self::Anna => {
                let (sin, cos) = turn.to_radians().sin_cos();
                let down = y.mul_add(cos, -x * sin);
                let (purple, blue) = ([156.0, 121.0, 211.0], [121.0, 224.0, 216.0]);
                shade(purple, blue, (down + 0.8) / 1.6)
            }
            Self::Shiroko => shade([115.0, 254.0, 254.0], [195.0, 254.0, 254.0], y / 0.7),
            Self::Midori => shade(
                [174.0, 253.0, 187.0],
                [198.0, 252.0, 194.0],
                f32::midpoint(y, 1.0),
            ),
            Self::Momoi => shade(
                [252.0, 162.0, 195.0],
                [251.0, 191.0, 199.0],
                f32::midpoint(y, 1.0),
            ),
            Self::Junko(palette) => {
                let hue = match palette {
                    Palette::Red => 342.0,
                    other => other.hues().0,
                };
                // Deep down to the middle, paler below it; a little short of full light.
                let paler = ((y - 0.02) / 0.7).clamp(0.0, 1.0);
                super::hsv(hue, 0.24f32.mul_add(-paler, 0.77)).map(|channel| channel * 0.95)
            }
        }
    }
}

/// Samples a dot is drawn from, across and down.
const SAMPLES: u32 = 8;
/// A halo's lines are kept at least this many of the cursor's dots wide.
const THINNEST: f32 = 1.5;

/// (`x`, `y`) turned back onto the nearest of `spokes` spokes, the first pointing up:
/// how far along that spoke it is, and how far to its side.
fn spoke(x: f32, y: f32, spokes: f32) -> (f32, f32) {
    let step = std::f32::consts::TAU / spokes;
    let (sin, cos) = ((x.atan2(-y) / step).round() * step).sin_cos();
    (x.mul_add(sin, -y * cos), x.mul_add(cos, y * sin))
}

/// How far inside a bar lying on a spoke the point (`along`, `aside`) of `spoke` is:
/// the bar runs between two distances from the middle and is half as wide as `half`
/// at each end.
fn bar((along, aside): (f32, f32), (from, to): (f32, f32), half: (f32, f32)) -> f32 {
    let wide = (half.1 - half.0).mul_add(((along - from) / (to - from)).clamp(0.0, 1.0), half.0);
    (along - from).min(to - along).min(wide - aside.abs())
}

/// The outline of Junko's point, as (aside, down) in the picture's pixels from the
/// halo's middle: its tip at the top, widest a little below it, a long tail down.
static POINT: LazyLock<Vec<(f32, f32)>> = LazyLock::new(|| {
    let side = [
        (3.5, -150.5),
        (7.5, -138.5),
        (11.5, -126.5),
        (17.0, -114.5),
        (24.5, -102.5),
        (33.0, -91.5),
        (26.5, -78.5),
        (21.0, -66.5),
        (16.5, -54.5),
        (12.5, -42.5),
        (9.0, -30.5),
        (6.5, -18.5),
        (4.0, -6.5),
        (2.5, 5.5),
    ];
    std::iter::once((0.0, -159.5))
        .chain(side.iter().copied())
        .chain(std::iter::once((0.0, 16.5)))
        .chain(side.iter().rev().map(|&(aside, down)| (-aside, down)))
        .collect()
});
/// The barbs at the ends of Junko's right arc, as `POINT`: one standing up from its
/// upper end, and an arrowhead down from its lower end.
const UPPER_BARB: [(f32, f32); 3] = [(35.0, -53.5), (35.0, -16.5), (20.5, -16.5)];
const LOWER_BARB: [(f32, f32); 6] = [
    (11.0, 105.5),
    (19.5, 114.5),
    (26.5, 127.5),
    (29.0, 159.5),
    (13.5, 133.5),
    (10.5, 128.5),
];

/// The outline of Midori's star on its spoke, as (aside, along) in the picture's
/// pixels from the halo's middle: a long point inward, a short one outward, their
/// sides curving in (measured every few pixels along the spoke).
static STAR: LazyLock<Vec<(f32, f32)>> = LazyLock::new(|| {
    let side = [
        (50.0, 2.0),
        (60.0, 3.0),
        (70.0, 4.5),
        (80.0, 6.0),
        (90.0, 8.5),
        (104.0, 13.0),
        (112.0, 16.5),
        (118.0, 20.5),
        (124.0, 22.0),
        (130.0, 18.5),
        (135.0, 12.0),
        (140.0, 8.0),
        (145.0, 4.0),
    ];
    std::iter::once((0.0, 34.75))
        .chain(side.iter().map(|&(along, half)| (half, along)))
        .chain(std::iter::once((0.0, 151.5)))
        .chain(side.iter().rev().map(|&(along, half)| (-half, along)))
        .collect()
});

#[cfg(test)]
mod tests {
    use super::*;

    const SIDE: u32 = 256;

    /// The halo drawn big, its colours turned by `turn`.
    fn big(halo: Halo, turn: f32) -> Vec<[u8; 4]> {
        halo.drawn(SIDE, (128.0, 128.0), 256.0, &[turn]).remove(0)
    }

    /// The dot `far` of the halo's reach from its middle, `degrees` clockwise of up.
    fn at(picture: &[[u8; 4]], far: f32, degrees: f32) -> [u8; 4] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        // The halo reaches a little short of the picture's side: its edge is outside it.
        let reach = 128.0 - 1.75;
        let column = super::super::to_byte_range(128.0 + far * reach * sin, 255.0);
        let row = super::super::to_byte_range(128.0 - far * reach * cos, 255.0);
        picture[usize::try_from(row * SIDE + column).unwrap()]
    }

    /// How many separate stretches of the halo's own colour a circle `far` out crosses.
    fn stretches(picture: &[[u8; 4]], far: f32) -> usize {
        let lit = |tenth: u16| {
            let [r, g, b, a] = at(picture, far, f32::from(tenth) / 10.0);
            a == 255 && r.max(g).max(b) > 150
        };
        let starts = (0..3600)
            .filter(|&d| lit(d) && !lit((d + 3599) % 3600))
            .count();
        if starts == 0 && lit(0) { 1 } else { starts }
    }

    fn navy(dot: [u8; 4]) -> bool {
        dot[3] > 30 && dot[..3].iter().all(|&c| c < 110)
    }

    /// Anna's halo is a gear: fourteen teeth outside its ring, six on the small ring
    /// inside, and a hole in the middle.
    #[test]
    fn annas_halo_is_a_gear() {
        let gear = big(Halo::Anna, 0.0);
        assert_eq!(stretches(&gear, 0.92), 14);
        assert_eq!(stretches(&gear, 0.75), 1, "the ring is whole");
        assert_eq!(stretches(&gear, 0.375), 6);
        assert_eq!(at(&gear, 0.0, 0.0)[3], 0);
        assert_eq!(at(&gear, 0.55, 40.0)[3], 0, "clear between the rings");
    }

    /// While a program is busy the gear stays as it is and its colours go round it
    /// (the user's of 2026-11-10: the picture is not turned, the colour in it flows):
    /// purple at the top and light blue at the bottom, the other way half a turn later.
    #[test]
    fn annas_colours_go_round_a_gear_that_stays() {
        let frames = Halo::Anna.drawn(SIDE, (128.0, 128.0), 256.0, &[0.0, 90.0, 180.0]);
        let alpha = |frame: &[[u8; 4]]| frame.iter().map(|dot| dot[3]).collect::<Vec<_>>();
        assert_eq!(alpha(&frames[0]), alpha(&frames[1]));
        assert_eq!(alpha(&frames[0]), alpha(&frames[2]));
        let purple = |[r, g, _, _]: [u8; 4]| r > g + 20;
        let blue = |[r, g, _, _]: [u8; 4]| g > r + 60;
        assert!(purple(at(&frames[0], 0.75, 0.0)) && blue(at(&frames[0], 0.75, 180.0)));
        assert!(purple(at(&frames[1], 0.75, 90.0)) && blue(at(&frames[1], 0.75, 270.0)));
        assert!(purple(at(&frames[2], 0.75, 180.0)) && blue(at(&frames[2], 0.75, 0.0)));
    }

    /// Shiroko's is a sight: two light-blue rings round a clear middle, and four marks
    /// that reach in and out of the outer ring.
    #[test]
    fn shirokos_halo_is_a_sight() {
        let sight = big(Halo::Shiroko, 0.0);
        assert_eq!(at(&sight, 0.0, 0.0)[3], 0);
        for far in [0.36, 0.76] {
            assert_eq!(stretches(&sight, far), 1, "a ring at {far}");
            let [r, g, b, _] = at(&sight, far, 45.0);
            assert!(g > 240 && b > 240 && r < 160, "{far}: {:?}", [r, g, b]);
        }
        assert_eq!(stretches(&sight, 0.55), 0, "clear between the rings");
        assert_eq!(stretches(&sight, 0.62), 4, "marks inside the outer ring");
        assert_eq!(stretches(&sight, 0.93), 4, "marks outside it");
    }

    /// Midori's has two thin rings and four stars across them whose long points reach
    /// far inside the rings; Momoi's is the same shape in pink, Midori's green.
    #[test]
    fn midoris_and_momois_halos_are_stars_across_two_rings() {
        let (green, pink) = (big(Halo::Midori, 0.0), big(Halo::Momoi, 0.0));
        let shape = |picture: &[[u8; 4]]| picture.iter().map(|dot| dot[3]).collect::<Vec<_>>();
        assert_eq!(shape(&green), shape(&pink));
        assert_eq!(stretches(&green, 0.65), 1, "the inner ring");
        assert_eq!(stretches(&green, 0.835), 1, "the outer ring");
        assert_eq!(stretches(&green, 0.4), 4, "the long points");
        assert_eq!(stretches(&green, 0.96), 4, "the short points");
        assert_eq!(at(&green, 0.0, 0.0)[3], 0);
        let [r, g, b, _] = at(&green, 0.65, 45.0);
        assert!(g > r + 40 && g > b + 40, "{:?}", [r, g, b]);
        let [r, g, b, _] = at(&pink, 0.65, 45.0);
        assert!(r > g + 40 && r > b + 30, "{:?}", [r, g, b]);
    }

    /// A pale halo shows over a pale page by a navy edge round it, outside its own
    /// lines, as the arrow has: out of a ring comes its colour, then navy, then nothing.
    #[test]
    fn a_navy_edge_runs_round_the_halo() {
        let sight = big(Halo::Shiroko, 0.0);
        let leaving = |from: f32, to: f32| {
            let mut seen = Vec::new();
            for step in 0..=60u8 {
                let far = (to - from).mul_add(f32::from(step) / 60.0, from);
                let dot = at(&sight, far, 45.0);
                let kind = if dot[3] == 0 {
                    "clear"
                } else if navy(dot) {
                    "navy"
                } else if dot[3] == 255 && dot[1] > 240 {
                    "blue"
                } else {
                    continue;
                };
                if seen.last() != Some(&kind) {
                    seen.push(kind);
                }
            }
            seen
        };
        assert_eq!(leaving(0.76, 0.9), ["blue", "navy", "clear"]);
        assert_eq!(leaving(0.76, 0.62), ["blue", "navy", "clear"]);
    }

    /// At a cursor's size the thin rings keep a dot and a half of their own colour and
    /// are not lost between their edges: down a column clear of the stars, the four
    /// crossings of the two rings add up to six dots of green, not the four they would
    /// at their true width.
    #[test]
    fn thin_lines_show_at_a_cursors_size() {
        let small = Halo::Midori.drawn(40, (20.0, 20.0), 40.0, &[0.0]).remove(0);
        // How much of a dot is the halo's own green, the rest navy or clear.
        let own =
            |[_, g, _, a]: [u8; 4]| (f32::from(g) - 62.0).max(0.0) / 190.0 * f32::from(a) / 255.0;
        let green: f32 = (0..40usize).map(|row| own(small[row * 40 + 25])).sum();
        assert!(green > 5.8, "{green} dots of green down the column");
    }

    /// Junko's is taller than wide: a long point up, a dot under it, and an arc at each
    /// side of the dot.
    #[test]
    fn junkos_halo_is_a_point_over_a_dot_between_two_arcs() {
        let halo = big(Halo::Junko(Palette::Red), 0.0);
        let lit = |far: f32, degrees: f32| at(&halo, far, degrees)[3] == 255;
        assert!(lit(0.5, 0.0) && lit(0.95, 0.0), "the point reaches the top");
        assert!(lit(0.32, 180.0), "the dot");
        assert_eq!(at(&halo, 0.155, 180.0)[3], 0, "clear between point and dot");
        assert!(
            lit(0.553, 124.1) && lit(0.553, 235.9),
            "an arc at each side"
        );
        assert_eq!(at(&halo, 0.4, 141.0)[3], 0, "clear between dot and arc");
        assert_eq!(at(&halo, 0.6, 90.0)[3], 0, "narrower than tall");
        assert_eq!(at(&halo, 0.9, 180.0)[3], 0, "open at the bottom");
    }

    /// Junko's halo is drawn in each colour of the highlight (the user's of
    /// 2026-11-10): the same shape, red as her own, green or blue.
    #[test]
    fn junkos_halo_comes_in_the_highlights_three_colours() {
        let of = |palette| big(Halo::Junko(palette), 0.0);
        let (red, green, blue) = (of(Palette::Red), of(Palette::Green), of(Palette::Blue));
        let shape = |picture: &[[u8; 4]]| picture.iter().map(|dot| dot[3]).collect::<Vec<_>>();
        assert_eq!(shape(&red), shape(&green));
        assert_eq!(shape(&red), shape(&blue));
        let [r, g, b, _] = at(&red, 0.5, 0.0);
        assert!(r > g + 100 && r > b + 80, "red {:?}", [r, g, b]);
        let [r, g, b, _] = at(&green, 0.5, 0.0);
        assert!(g > r + 100 && g > b + 80, "green {:?}", [r, g, b]);
        let [r, g, b, _] = at(&blue, 0.5, 0.0);
        assert!(b > r + 100 && g > r + 80, "blue {:?}", [r, g, b]);
    }

    /// Each colour of the highlight aims with its own halo.
    #[test]
    fn each_colour_aims_with_its_halo() {
        assert_eq!(Halo::aim(Palette::Blue), Halo::Shiroko);
        assert_eq!(Halo::aim(Palette::Green), Halo::Midori);
        assert_eq!(Halo::aim(Palette::Red), Halo::Momoi);
    }
}
