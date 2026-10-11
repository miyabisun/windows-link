//! A real USB mouse that windows-link drives over a serial line, and the wheel
//! requests the API turns into it.
//!
//! Programs running as administrator ignore input that a normal process
//! injects (`SendInput`), but take input from a mouse. A small board (an
//! RP2350 running `firmware/mouse`) is such a mouse, and also a serial port
//! that takes one command per line:
//!
//! - `?` answers `windows-link mouse <version>`, to find the board among ports;
//! - `W <n>` turns the wheel `n` notches (negative scrolls down) and answers `ok`.

#[cfg(windows)]
pub mod windows;

use serde::Deserialize;

use crate::config::{ButtonSpec, Config};

/// What the board answers to `?`, followed by its version.
pub const GREETING: &str = "windows-link mouse";
/// Most notches one request may turn.
pub const MAX_NOTCHES: i32 = 20;

/// The command line that turns the wheel.
pub fn wheel_command(notches: i32) -> String {
    format!("W {notches}\n")
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

/// `POST /buttons/{id}/wheel`: where in the app's window (fractions of its client
/// area) and how many notches.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct WheelRequest {
    pub x: f64,
    pub y: f64,
    pub notches: i32,
}

/// A checked wheel request for one program's window.
#[derive(Debug, Clone, PartialEq)]
pub struct Wheel {
    pub process: String,
    pub x: f64,
    pub y: f64,
    pub notches: i32,
}

#[derive(Debug, PartialEq)]
pub enum WheelError {
    NotFound,
    /// Not an app button with a `process`: only configured apps can be scrolled.
    NotScrollable,
    /// An `automation.hold` button is on: the user is using the PC.
    Held,
    Invalid(String),
}

/// Check a wheel request against the configuration before anything moves.
pub fn plan(
    config: &Config,
    id: &str,
    request: WheelRequest,
    held: bool,
) -> Result<Wheel, WheelError> {
    let button = config
        .buttons
        .iter()
        .find(|b| b.id == id)
        .ok_or(WheelError::NotFound)?;
    let ButtonSpec::AppLaunch {
        process: Some(process),
        ..
    } = &button.spec
    else {
        return Err(WheelError::NotScrollable);
    };
    if held {
        return Err(WheelError::Held);
    }
    let WheelRequest { x, y, notches } = request;
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(WheelError::Invalid(
            "x and y must be between 0 and 1".into(),
        ));
    }
    if notches == 0 || notches.abs() > MAX_NOTCHES {
        return Err(WheelError::Invalid(format!(
            "notches must be 1 to {MAX_NOTCHES} in either direction"
        )));
    }
    Ok(Wheel {
        process: process.clone(),
        x,
        y,
        notches,
    })
}

#[derive(Debug, PartialEq)]
pub enum ScrollError {
    /// No board answers on any port.
    NoMouse,
    /// The program has no window to bring forward.
    NoWindow,
    /// Something else covers the point in the window.
    Covered,
    Failed(String),
}

/// Brings a program's window forward, puts the cursor on a point of it, turns the
/// mouse wheel there and puts the cursor back.
pub trait Pointer: Send + Sync + 'static {
    fn scroll(&self, wheel: &Wheel) -> Result<(), ScrollError>;
}

/// Without a board, every scroll answers that there is no mouse.
pub struct NoMouse;

impl Pointer for NoMouse {
    fn scroll(&self, _: &Wheel) -> Result<(), ScrollError> {
        Err(ScrollError::NoMouse)
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::{Pointer, ScrollError, Wheel};

    /// Records scrolls, or fails with `error` when set.
    #[derive(Default)]
    pub struct FakePointer {
        pub scrolled: Mutex<Vec<Wheel>>,
        pub error: Mutex<Option<ScrollError>>,
    }

    impl Pointer for FakePointer {
        fn scroll(&self, wheel: &Wheel) -> Result<(), ScrollError> {
            if let Some(error) = self.error.lock().unwrap().take() {
                return Err(error);
            }
            self.scrolled.lock().unwrap().push(wheel.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WheelError, WheelRequest, candidate_ports, is_greeting, plan, wheel_command};
    use crate::config;

    #[test]
    fn a_wheel_command_is_one_line() {
        assert_eq!(wheel_command(-3), "W -3\n");
        assert_eq!(wheel_command(2), "W 2\n");
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
        assert_eq!(wheel.notches, -3);
        assert_eq!(
            plan(&config, "nope", request(0.5, 0.5, 1), false),
            Err(WheelError::NotFound)
        );
        assert_eq!(
            plan(&config, "web", request(0.5, 0.5, 1), false),
            Err(WheelError::NotScrollable)
        );
        assert_eq!(
            plan(&config, "ba", request(0.5, 0.5, 1), true),
            Err(WheelError::Held)
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
                Err(WheelError::Invalid(_))
            ));
        }
    }
}
