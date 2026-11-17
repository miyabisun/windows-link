//! A real USB mouse that windows-link drives over a serial line, and the click,
//! drag and wheel requests the API turns into it.
//!
//! Programs running as administrator ignore input that a normal process
//! injects (`SendInput`), but take input from a mouse. A small board (an
//! RP2350 running `firmware/mouse`) is such a mouse, and also a serial port
//! that takes one command per line:
//!
//! - `?` answers `windows-link mouse <version>`, to find the board among ports;
//! - `W <n>` turns the wheel `n` notches (negative scrolls down) and answers `ok`;
//! - `B <mask>` holds down the buttons of `mask` (1 left, 2 right, 4 middle) and
//!   lets the others go, and answers `ok`. The board lets go by itself when no
//!   command follows for a few seconds;
//! - `M <x> <y>` moves the mouse `x` counts to the right and `y` down (each -127 to
//!   127) and answers `ok`.
//!
//! The cursor is put on the point by Windows (`SetCursorPos`) where Windows does it;
//! where it does not (a program run as administrator is at the front), the board
//! moves the cursor there (`step_toward`).

#[cfg(windows)]
pub mod windows;

use serde::Deserialize;

use crate::config::{ButtonSpec, Config};

/// What the board answers to `?`, followed by its version.
pub const GREETING: &str = "windows-link mouse";
/// Most notches one request may turn.
pub const MAX_NOTCHES: i32 = 20;
/// Most presses one click request may make (a double click).
pub const MAX_CLICKS: u8 = 2;

/// The command line that turns the wheel.
pub fn wheel_command(notches: i32) -> String {
    format!("W {notches}\n")
}

/// The command line that holds down the buttons of `mask` and lets the others go.
pub fn buttons_command(mask: u8) -> String {
    format!("B {mask}\n")
}

/// Most counts the board moves at once along an axis.
pub const MAX_MOVE: i32 = 127;

/// The command line that moves the mouse `counts` to the right and down.
pub fn move_command(counts: (i32, i32)) -> String {
    format!("M {} {}\n", counts.0, counts.1)
}

/// How many pixels the cursor goes for a count the board moves, along each axis, as
/// measured so far: Windows takes the cursor farther a count in a fast move than in a
/// slow one, and how far depends on the user's pointer settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    pub x: f64,
    pub y: f64,
}

impl Default for Pace {
    fn default() -> Self {
        Self { x: 1.0, y: 1.0 }
    }
}

impl Pace {
    /// One odd reading must not throw the next steps far off.
    const SLOWEST: f64 = 0.25;
    const FASTEST: f64 = 8.0;

    /// The pace after the board moved `sent` counts and the cursor went `went` pixels.
    /// Nothing is learnt along an axis that was not moved, where the cursor stayed (a
    /// screen edge) or where it went the other way (the user's hand on the mouse).
    #[must_use]
    pub fn after(self, sent: (i32, i32), went: (i32, i32)) -> Self {
        let learn = |pace: f64, sent: i32, went: i32| {
            if sent != 0 && went != 0 && (sent > 0) == (went > 0) {
                (f64::from(went) / f64::from(sent)).clamp(Self::SLOWEST, Self::FASTEST)
            } else {
                pace
            }
        };
        Self {
            x: learn(self.x, sent.0, went.0),
            y: learn(self.y, sent.1, went.1),
        }
    }
}

/// The counts of the board's next move from `cursor` toward `target`: as many as
/// `pace` says the way takes, no more than the board moves at once, and none once
/// there.
///
/// The board moves the cursor itself where Windows will not: while a program run as
/// administrator is at the front, `SetCursorPos` from a normal program leaves the
/// cursor where it is (measured on 2026-11-18 with Blue Archive).
#[allow(
    clippy::cast_possible_truncation,
    reason = "a way across the screen, in counts, fits in i32"
)]
pub fn step_toward(cursor: (i32, i32), target: (i32, i32), pace: Pace) -> (i32, i32) {
    let along = |from: i32, to: i32, pace: f64| {
        let left = to - from;
        let counts = (f64::from(left) / pace).round() as i32;
        // The last pixels still take a count.
        let counts = if counts == 0 { left.signum() } else { counts };
        counts.clamp(-MAX_MOVE, MAX_MOVE)
    };
    (
        along(cursor.0, target.0, pace.x),
        along(cursor.1, target.1, pace.y),
    )
}

/// What the board answered to a command.
#[derive(Debug, PartialEq)]
pub enum Reply {
    Done,
    /// The board read the command and would not do it (older firmware, say), with why.
    Refused(String),
    /// Nothing, or not an answer: the port may not be the board any more.
    Unclear,
}

/// Read the board's answer line.
pub fn reply(line: &str) -> Reply {
    let line = line.trim();
    if line == "ok" {
        Reply::Done
    } else if let Some(why) = line.strip_prefix("error ") {
        Reply::Refused(why.to_owned())
    } else {
        Reply::Unclear
    }
}

/// The point of a client area `size` wide and high at fractions `x` and `y` of it; the
/// far edge is the last pixel inside.
#[allow(
    clippy::cast_possible_truncation,
    reason = "x and y are fractions of the client size, which fits in i32"
)]
pub fn in_client(size: (i32, i32), x: f64, y: f64) -> (i32, i32) {
    let along = |extent: i32, part: f64| {
        ((f64::from(extent) * part).round() as i32)
            .min(extent - 1)
            .max(0)
    };
    (along(size.0, x), along(size.1, y))
}

/// The points the cursor visits after `from` while dragging to `to`: `steps` even
/// steps, the last one `to` itself.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a point between two screen points fits in i32"
)]
pub fn glide(from: (i32, i32), to: (i32, i32), steps: u32) -> Vec<(i32, i32)> {
    let between = |a: i32, b: i32, step: u32| {
        let part = f64::from(step) / f64::from(steps);
        (f64::from(a) + f64::from(b - a) * part).round() as i32
    };
    (1..=steps)
        .map(|step| (between(from.0, to.0, step), between(from.1, to.1, step)))
        .collect()
}

/// Whether a line read from a port is the board's greeting.
pub fn is_greeting(line: &str) -> bool {
    line.trim()
        .strip_prefix(GREETING)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// USB vendor IDs of boards that may run the mouse firmware: Raspberry Pi
/// (RP2040/RP2350 boards such as Waveshare's) and Adafruit (`CircuitPython`).
pub const VENDORS: [u16; 2] = [0x2E8A, 0x239A];

/// Serial ports worth asking for the greeting: USB ports of those vendors, so
/// unrelated devices are never written to.
pub fn candidate_ports(ports: &[(String, Option<u16>)]) -> Vec<String> {
    ports
        .iter()
        .filter(|(_, vendor)| vendor.is_some_and(|v| VENDORS.contains(&v)))
        .map(|(name, _)| name.clone())
        .collect()
}

/// A button of the mouse, as the API names it.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// The button's bit in the board's `B` command.
    pub fn mask(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
        }
    }
}

/// What the mouse does at a point of an app's window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// Turn the wheel (negative scrolls down).
    Wheel { notches: i32 },
    /// Press and let go `count` times.
    Click { button: MouseButton, count: u8 },
    /// Press, move to another point of the window and let go there.
    Drag {
        to_x: f64,
        to_y: f64,
        button: MouseButton,
    },
}

/// A request for the mouse: where in the app's window (fractions of its client area)
/// and what to do there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ask {
    pub x: f64,
    pub y: f64,
    pub motion: Motion,
}

/// `POST /buttons/{id}/wheel`: where in the app's window and how many notches.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct WheelRequest {
    pub x: f64,
    pub y: f64,
    pub notches: i32,
}

/// `POST /buttons/{id}/click`: where in the app's window, and which button how often
/// (the left one once unless told).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct ClickRequest {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub button: MouseButton,
    #[serde(default = "once")]
    pub count: u8,
}

fn once() -> u8 {
    1
}

/// `POST /buttons/{id}/drag`: from where to where in the app's window, with which
/// button held (the left one unless told).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct DragRequest {
    pub x: f64,
    pub y: f64,
    pub to_x: f64,
    pub to_y: f64,
    #[serde(default)]
    pub button: MouseButton,
}

impl From<WheelRequest> for Ask {
    fn from(WheelRequest { x, y, notches }: WheelRequest) -> Self {
        Self {
            x,
            y,
            motion: Motion::Wheel { notches },
        }
    }
}

impl From<ClickRequest> for Ask {
    fn from(click: ClickRequest) -> Self {
        Self {
            x: click.x,
            y: click.y,
            motion: Motion::Click {
                button: click.button,
                count: click.count,
            },
        }
    }
}

impl From<DragRequest> for Ask {
    fn from(drag: DragRequest) -> Self {
        Self {
            x: drag.x,
            y: drag.y,
            motion: Motion::Drag {
                to_x: drag.to_x,
                to_y: drag.to_y,
                button: drag.button,
            },
        }
    }
}

/// A checked request for one program's window.
#[derive(Debug, Clone, PartialEq)]
pub struct Gesture {
    pub process: String,
    pub x: f64,
    pub y: f64,
    pub motion: Motion,
}

#[derive(Debug, PartialEq)]
pub enum PlanError {
    NotFound,
    /// Not an app button with a `process`: the mouse only works in the windows of
    /// configured apps.
    NotAnApp,
    /// An `automation.hold` button is on: the user is using the PC.
    Held,
    Invalid(String),
}

/// Check a request for the mouse against the configuration before anything moves.
pub fn plan(
    config: &Config,
    id: &str,
    ask: impl Into<Ask>,
    held: bool,
) -> Result<Gesture, PlanError> {
    let button = config
        .buttons
        .iter()
        .find(|b| b.id == id)
        .ok_or(PlanError::NotFound)?;
    let ButtonSpec::AppLaunch {
        process: Some(process),
        ..
    } = &button.spec
    else {
        return Err(PlanError::NotAnApp);
    };
    if held {
        return Err(PlanError::Held);
    }
    let Ask { x, y, motion } = ask.into();
    let inside = |x: f64, y: f64| (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y);
    if !inside(x, y) {
        return Err(PlanError::Invalid("x and y must be between 0 and 1".into()));
    }
    match motion {
        Motion::Wheel { notches } if notches == 0 || notches.abs() > MAX_NOTCHES => {
            return Err(PlanError::Invalid(format!(
                "notches must be 1 to {MAX_NOTCHES} in either direction"
            )));
        }
        Motion::Click { count, .. } if count == 0 || count > MAX_CLICKS => {
            return Err(PlanError::Invalid(format!(
                "count must be 1 to {MAX_CLICKS}"
            )));
        }
        Motion::Drag { to_x, to_y, .. } if !inside(to_x, to_y) => {
            return Err(PlanError::Invalid(
                "to_x and to_y must be between 0 and 1".into(),
            ));
        }
        _ => {}
    }
    Ok(Gesture {
        process: process.clone(),
        x,
        y,
        motion,
    })
}

#[derive(Debug, PartialEq)]
pub enum MouseError {
    /// No board answers on any port.
    NoMouse,
    /// The program has no window to bring forward.
    NoWindow,
    /// Something else covers the point in the window.
    Covered,
    Failed(String),
}

/// Brings a program's window forward, puts the cursor on a point of it, clicks,
/// drags or turns the wheel there with the mouse board and puts the cursor back.
pub trait Pointer: Send + Sync + 'static {
    fn act(&self, gesture: &Gesture) -> Result<(), MouseError>;
}

/// Without a board, every request answers that there is no mouse.
pub struct NoMouse;

impl Pointer for NoMouse {
    fn act(&self, _: &Gesture) -> Result<(), MouseError> {
        Err(MouseError::NoMouse)
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::{Gesture, MouseError, Pointer};

    /// Records what the mouse was asked to do, or fails with `error` when set.
    #[derive(Default)]
    pub struct FakePointer {
        pub done: Mutex<Vec<Gesture>>,
        pub error: Mutex<Option<MouseError>>,
    }

    impl Pointer for FakePointer {
        fn act(&self, gesture: &Gesture) -> Result<(), MouseError> {
            if let Some(error) = self.error.lock().unwrap().take() {
                return Err(error);
            }
            self.done.lock().unwrap().push(gesture.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClickRequest, DragRequest, Motion, MouseButton, Pace, PlanError, Reply, WheelRequest,
        buttons_command, candidate_ports, glide, in_client, is_greeting, move_command, plan, reply,
        step_toward, wheel_command,
    };
    use crate::config;

    #[test]
    fn a_wheel_command_is_one_line() {
        assert_eq!(wheel_command(-3), "W -3\n");
        assert_eq!(wheel_command(2), "W 2\n");
    }

    #[test]
    fn a_buttons_command_tells_which_buttons_are_down() {
        assert_eq!(buttons_command(MouseButton::Left.mask()), "B 1\n");
        assert_eq!(buttons_command(MouseButton::Right.mask()), "B 2\n");
        assert_eq!(buttons_command(MouseButton::Middle.mask()), "B 4\n");
        assert_eq!(buttons_command(0), "B 0\n", "all let go");
    }

    #[test]
    fn a_move_command_tells_how_far_right_and_down() {
        assert_eq!(move_command((127, -3)), "M 127 -3\n");
        assert_eq!(move_command((0, 1)), "M 0 1\n");
    }

    #[test]
    fn a_step_toward_the_target_is_as_long_as_the_pace_says_within_the_boards_reach() {
        let even = Pace::default();
        assert_eq!(step_toward((100, 100), (100, 100), even), (0, 0), "there");
        assert_eq!(step_toward((0, 0), (40, -25), even), (40, -25));
        assert_eq!(
            step_toward((-1262, 1410), (720, 78), even),
            (127, -127),
            "no farther than the board moves at once"
        );
        // Windows takes the cursor three pixels a count at this speed: a third as many counts.
        let fast = Pace { x: 3.0, y: 3.0 };
        assert_eq!(step_toward((0, 0), (90, -30), fast), (30, -10));
        assert_eq!(
            step_toward((0, 0), (1, -1), fast),
            (1, -1),
            "the last pixel still takes a count"
        );
        assert_eq!(
            step_toward((5, 0), (5, 9), fast),
            (0, 3),
            "one axis at rest"
        );
    }

    #[test]
    fn the_pace_is_learnt_from_how_far_the_cursor_went() {
        let pace = Pace::default().after((127, -50), (381, -75));
        assert_eq!(pace, Pace { x: 3.0, y: 1.5 });
        // Nothing is learnt along an axis that was not moved, where the cursor stayed
        // (a screen edge) or where it went the other way (the user's hand).
        assert_eq!(pace.after((0, 10), (4, 0)), pace);
        assert_eq!(pace.after((10, 10), (-20, 15)), Pace { x: 3.0, y: 1.5 });
        // An odd reading does not throw the next steps far off.
        let wild = Pace::default().after((1, 1), (500, 0));
        assert_eq!(wild, Pace { x: 8.0, y: 1.0 });
        let slow = Pace::default().after((100, 1), (1, 1));
        assert_eq!(slow, Pace { x: 0.25, y: 1.0 });
    }

    /// A cursor as Windows moves it: fast moves go farther a count than slow ones, and
    /// the screen ends at its edges.
    fn glide_by(cursor: (i32, i32), counts: (i32, i32)) -> (i32, i32) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "pixels on a screen fit in i32"
        )]
        let along = |count: i32| {
            let speed = 0.6 + f64::from(count.abs()) / 40.0;
            (f64::from(count) * speed).round() as i32
        };
        (
            (cursor.0 + along(counts.0)).clamp(-2560, 2559),
            (cursor.1 + along(counts.1)).clamp(0, 1439),
        )
    }

    #[test]
    fn steps_and_pace_bring_a_speeding_cursor_onto_the_target() {
        for (start, target) in [
            ((-1262, 1410), (720, 78)),
            ((720, 78), (-1262, 1410)),
            ((100, 100), (103, 99)),
            ((0, 0), (2559, 1439)),
            ((2000, 700), (2000, 20)),
        ] {
            let (mut cursor, mut pace, mut steps) = (start, Pace::default(), 0);
            loop {
                let step = step_toward(cursor, target, pace);
                if step == (0, 0) {
                    break;
                }
                let now = glide_by(cursor, step);
                pace = pace.after(step, (now.0 - cursor.0, now.1 - cursor.1));
                cursor = now;
                steps += 1;
                assert!(steps < 60, "{start:?} to {target:?} does not settle");
            }
            assert_eq!(cursor, target);
        }
    }

    #[test]
    fn the_boards_answer_is_done_refused_or_unclear() {
        assert_eq!(reply("ok\r\n"), Reply::Done);
        assert_eq!(
            reply("error unknown command\n"),
            Reply::Refused("unknown command".into()),
            "a board with older firmware"
        );
        assert_eq!(reply(""), Reply::Unclear, "nothing came in time");
        assert_eq!(reply("okay"), Reply::Unclear);
    }

    #[test]
    fn a_point_of_the_window_is_a_fraction_of_its_client_area() {
        assert_eq!(in_client((1920, 1080), 0.5, 0.25), (960, 270));
        assert_eq!(in_client((1280, 720), 0.0, 0.0), (0, 0));
        assert_eq!(
            in_client((1921, 1080), 0.5, 1.0),
            (961, 1079),
            "the far edge is the last pixel inside, not the first one outside"
        );
        assert_eq!(in_client((0, 0), 1.0, 1.0), (0, 0), "a window with no area");
    }

    #[test]
    fn a_drag_glides_in_even_steps_and_ends_on_its_target() {
        assert_eq!(
            glide((0, 100), (40, 80), 4),
            [(10, 95), (20, 90), (30, 85), (40, 80)]
        );
        assert_eq!(
            glide((5, 5), (5, 5), 2),
            [(5, 5), (5, 5)],
            "a drag that goes nowhere still ends on its target"
        );
        assert_eq!(glide((0, 0), (-3, 1), 1), [(-3, 1)]);
    }

    #[test]
    fn a_click_is_a_left_single_click_unless_told() {
        let click: ClickRequest = serde_json::from_str(r#"{"x": 0.5, "y": 0.5}"#).unwrap();
        assert_eq!(click.button, MouseButton::Left);
        assert_eq!(click.count, 1);
        let click: ClickRequest =
            serde_json::from_str(r#"{"x": 0.5, "y": 0.5, "button": "right", "count": 2}"#).unwrap();
        assert_eq!(click.button, MouseButton::Right);
        assert_eq!(click.count, 2);
        assert!(
            serde_json::from_str::<ClickRequest>(r#"{"x": 0.5, "y": 0.5, "button": "side"}"#)
                .is_err()
        );
        let drag: DragRequest =
            serde_json::from_str(r#"{"x": 0.1, "y": 0.2, "to_x": 0.3, "to_y": 0.4}"#).unwrap();
        assert_eq!(drag.button, MouseButton::Left);
    }

    #[test]
    fn the_board_is_known_by_its_greeting() {
        assert!(is_greeting("windows-link mouse 1\r\n"));
        assert!(is_greeting("windows-link mouse"));
        assert!(!is_greeting("windows-link mousetrap"));
        assert!(!is_greeting("ok"));
        assert!(!is_greeting(""));
    }

    #[test]
    fn only_ports_of_known_boards_are_asked() {
        let ports = vec![
            ("COM3".to_owned(), Some(0x2E8A)),
            ("COM4".to_owned(), Some(0x0403)),
            ("COM5".to_owned(), None),
            ("COM7".to_owned(), Some(0x239A)),
        ];
        assert_eq!(candidate_ports(&ports), ["COM3", "COM7"]);
    }

    const CONFIG: &str = "buttons:
  - id: ba
    label: BA
    type: app.launch
    target: launcher.exe
    process: BlueArchive.exe
  - id: web
    label: Web
    type: app.launch
    target: https://example.com
";

    fn request(x: f64, y: f64, notches: i32) -> WheelRequest {
        WheelRequest { x, y, notches }
    }

    #[test]
    fn a_wheel_needs_a_running_app_button_and_sane_values() {
        let config = config::parse(CONFIG).unwrap();
        let wheel = plan(&config, "ba", request(0.75, 0.55, -3), false).unwrap();
        assert_eq!(wheel.process, "BlueArchive.exe");
        assert_eq!(wheel.motion, Motion::Wheel { notches: -3 });
        assert_eq!(
            plan(&config, "nope", request(0.5, 0.5, 1), false),
            Err(PlanError::NotFound)
        );
        assert_eq!(
            plan(&config, "web", request(0.5, 0.5, 1), false),
            Err(PlanError::NotAnApp)
        );
        assert_eq!(
            plan(&config, "ba", request(0.5, 0.5, 1), true),
            Err(PlanError::Held)
        );
        for bad in [
            request(1.5, 0.5, 1),
            request(0.5, -0.1, 1),
            request(0.5, 0.5, 0),
            request(0.5, 0.5, 21),
            request(0.5, 0.5, -21),
        ] {
            assert!(matches!(
                plan(&config, "ba", bad, false),
                Err(PlanError::Invalid(_))
            ));
        }
    }

    fn click(x: f64, y: f64, count: u8) -> ClickRequest {
        ClickRequest {
            x,
            y,
            button: MouseButton::Right,
            count,
        }
    }

    #[test]
    fn a_click_needs_an_app_button_a_point_in_its_window_and_one_or_two_presses() {
        let config = config::parse(CONFIG).unwrap();
        let planned = plan(&config, "ba", click(0.25, 1.0, 2), false).unwrap();
        assert_eq!(planned.process, "BlueArchive.exe");
        assert_eq!((planned.x, planned.y), (0.25, 1.0));
        assert_eq!(
            planned.motion,
            Motion::Click {
                button: MouseButton::Right,
                count: 2
            }
        );
        assert_eq!(
            plan(&config, "web", click(0.5, 0.5, 1), false),
            Err(PlanError::NotAnApp)
        );
        assert_eq!(
            plan(&config, "ba", click(0.5, 0.5, 1), true),
            Err(PlanError::Held)
        );
        for bad in [click(0.5, 1.01, 1), click(0.5, 0.5, 0), click(0.5, 0.5, 3)] {
            assert!(matches!(
                plan(&config, "ba", bad, false),
                Err(PlanError::Invalid(_))
            ));
        }
    }

    fn drag(to_x: f64, to_y: f64) -> DragRequest {
        DragRequest {
            x: 0.5,
            y: 0.8,
            to_x,
            to_y,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn a_drag_starts_and_ends_inside_the_window() {
        let config = config::parse(CONFIG).unwrap();
        let planned = plan(&config, "ba", drag(0.5, 0.2), false).unwrap();
        assert_eq!((planned.x, planned.y), (0.5, 0.8));
        assert_eq!(
            planned.motion,
            Motion::Drag {
                to_x: 0.5,
                to_y: 0.2,
                button: MouseButton::Left
            }
        );
        assert_eq!(
            plan(&config, "ba", drag(0.5, 0.2), true),
            Err(PlanError::Held)
        );
        for bad in [drag(1.2, 0.2), drag(0.5, -0.5)] {
            assert!(matches!(
                plan(&config, "ba", bad, false),
                Err(PlanError::Invalid(_))
            ));
        }
    }
}
