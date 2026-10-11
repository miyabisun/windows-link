//! The mouse board on a serial port, and the cursor work around its wheel.

use std::{
    io::{BufRead, BufReader},
    sync::{Arc, Mutex, PoisonError},
    thread::sleep,
    time::Duration,
};

use serialport::{ClearBuffer, SerialPort, SerialPortType};
use windows::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetClientRect, GetCursorPos, GetForegroundWindow, SetCursorPos,
        WindowFromPoint,
    },
};

use super::{Pointer, ScrollError, Wheel, candidate_ports, is_greeting, wheel_command};
use crate::launch::{Launcher, Program, windows::main_window_of};

const BAUD: u32 = 115_200;
/// The board answers a command within this time (turning 20 notches included).
const ANSWER: Duration = Duration::from_secs(1);
/// The app reads the wheel on its next frames, while the cursor is still over it.
const SETTLE: Duration = Duration::from_millis(200);

pub struct WindowsPointer {
    launcher: Arc<dyn Launcher>,
    /// The board's port once found; looked for again when it stops answering.
    board: Mutex<Option<Box<dyn SerialPort>>>,
}

impl WindowsPointer {
    pub fn new(launcher: Arc<dyn Launcher>) -> Self {
        Self {
            launcher,
            board: Mutex::new(None),
        }
    }

    /// The first port of a known board vendor that greets as the mouse board.
    fn find_board() -> Option<Box<dyn SerialPort>> {
        let ports = serialport::available_ports().ok()?;
        let listed: Vec<(String, Option<u16>)> = ports
            .into_iter()
            .map(|port| {
                let vendor = match port.port_type {
                    SerialPortType::UsbPort(usb) => Some(usb.vid),
                    _ => None,
                };
                (port.port_name, vendor)
            })
            .collect();
        candidate_ports(&listed).into_iter().find_map(|name| {
            let mut port = serialport::new(&name, BAUD).timeout(ANSWER).open().ok()?;
            ask(&mut *port, "?\n").filter(|line| is_greeting(line))?;
            Some(port)
        })
    }

    /// Turn the wheel, finding the board again once if it was unplugged.
    fn turn(&self, notches: i32) -> Result<(), ScrollError> {
        let mut board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..2 {
            if board.is_none() {
                *board = Self::find_board();
            }
            let Some(port) = board.as_mut() else {
                return Err(ScrollError::NoMouse);
            };
            if ask(&mut **port, &wheel_command(notches)).is_some_and(|line| line.trim() == "ok") {
                return Ok(());
            }
            *board = None;
        }
        Err(ScrollError::NoMouse)
    }
}

/// Send one command line and read the answer line.
fn ask(port: &mut dyn SerialPort, line: &str) -> Option<String> {
    port.clear(ClearBuffer::Input).ok()?;
    port.write_all(line.as_bytes()).ok()?;
    let mut answer = String::new();
    BufReader::new(port).read_line(&mut answer).ok()?;
    Some(answer)
}

impl Pointer for WindowsPointer {
    fn scroll(&self, wheel: &Wheel) -> Result<(), ScrollError> {
        // Without a board nothing is brought forward or moved.
        {
            let mut board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
            if board.is_none() {
                *board = Self::find_board();
            }
            if board.is_none() {
                return Err(ScrollError::NoMouse);
            }
        }
        let program = Program::Exe(wheel.process.clone());
        if !self.launcher.focus(&program).map_err(ScrollError::Failed)? {
            return Err(ScrollError::NoWindow);
        }
        let window = main_window_of(&wheel.process).ok_or(ScrollError::NoWindow)?;
        if unsafe { GetForegroundWindow() } != window {
            return Err(ScrollError::Failed(format!(
                "{} did not come to the front",
                wheel.process
            )));
        }
        let mut client = RECT::default();
        unsafe { GetClientRect(window, &raw mut client) }
            .map_err(|err| ScrollError::Failed(err.to_string()))?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "x and y are fractions of the client size, which fits in i32"
        )]
        let mut point = POINT {
            x: (f64::from(client.right) * wheel.x).round() as i32,
            y: (f64::from(client.bottom) * wheel.y).round() as i32,
        };
        let _ = unsafe { ClientToScreen(window, &raw mut point) };

        // The wheel goes to whatever is under the cursor, so the cursor visits the
        // point and then goes back where the user left it.
        let mut saved = POINT::default();
        let _ = unsafe { GetCursorPos(&raw mut saved) };
        let _ = unsafe { SetCursorPos(point.x, point.y) };
        let result = if unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) } == window {
            let turned = self.turn(wheel.notches);
            sleep(SETTLE);
            turned
        } else {
            Err(ScrollError::Covered)
        };
        let _ = unsafe { SetCursorPos(saved.x, saved.y) };
        result
    }
}
