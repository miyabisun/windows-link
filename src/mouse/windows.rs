//! The mouse board on a serial port, and the cursor work around what it does.

use std::{
    io::{BufRead, BufReader},
    sync::{Arc, Mutex, PoisonError},
    thread::sleep,
    time::{Duration, Instant},
};

use serialport::{ClearBuffer, SerialPort, SerialPortType};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetClientRect, GetCursorPos, GetForegroundWindow, SetCursorPos,
        WindowFromPoint,
    },
};

use super::{
    Gesture, Motion, MouseError, Pace, Pointer, Reply, buttons_command, candidate_ports, glide,
    in_client, is_greeting, move_command, reply, step_toward, wheel_command,
};
use crate::{
    launch::{Launcher, Program, windows::main_window_of},
    touch::windows::board_at_work,
};

const BAUD: u32 = 115_200;
/// The board answers a command within this time (turning 20 notches included).
const ANSWER: Duration = Duration::from_secs(1);
/// The app reads the mouse on its next frames, while the cursor is still over it.
const SETTLE: Duration = Duration::from_millis(200);
/// The app sees the cursor arrive before anything is pressed or turned, and a button
/// stays down this long in a click (and up between the two presses of a double click).
/// Blue Archive missed a click of 50 ms made at once on arrival; it took one made
/// 300 ms later, and one of 150 ms (measured on 2026-11-18).
const ARRIVED: Duration = Duration::from_millis(100);
const PRESS: Duration = Duration::from_millis(100);
/// How long a window may take to come to the front once asked, and how often to look.
const FRONT_WAIT: Duration = Duration::from_secs(1);
const FRONT_LOOK: Duration = Duration::from_millis(20);
/// The steps the cursor takes in a drag, and the pause after each.
const DRAG_STEPS: u32 = 30;
const DRAG_STEP: Duration = Duration::from_millis(10);
/// Windows has moved the cursor this long after the board answered a move (the board
/// reports to the PC every 8 ms).
const MOVED: Duration = Duration::from_millis(20);
/// The most moves the board makes to reach a point, and the most in a row that leave
/// the cursor where it is, before giving up.
const MOVES: u32 = 300;
const STILL_MOVES: u32 = 25;

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

    /// Whether a board is there, looking for one when none is known.
    fn has_board(&self) -> bool {
        let mut board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        if board.is_none() {
            *board = Self::find_board();
        }
        board.is_some()
    }

    /// Give the board a command, finding the board again once if it was unplugged.
    fn send(&self, command: &str) -> Result<(), MouseError> {
        let mut board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..2 {
            if board.is_none() {
                *board = Self::find_board();
            }
            let Some(port) = board.as_mut() else {
                return Err(MouseError::NoMouse);
            };
            match ask(&mut **port, command).map_or(Reply::Unclear, |line| reply(&line)) {
                Reply::Done => return Ok(()),
                Reply::Refused(why) => {
                    return Err(MouseError::Failed(format!(
                        "the mouse board refused `{}` ({why}); it may run an older firmware/mouse",
                        command.trim()
                    )));
                }
                Reply::Unclear => *board = None,
            }
        }
        Err(MouseError::NoMouse)
    }

    /// Put the cursor on `target`. Windows moves it where it may; while a program run as
    /// administrator is at the front it may not (the cursor stays where it is), and the
    /// board moves it as a mouse does, step by step until it is there.
    fn put_cursor(&self, target: POINT) -> Result<(), MouseError> {
        // SAFETY: SetCursorPos only takes coordinates.
        let _ = unsafe { SetCursorPos(target.x, target.y) };
        let target = (target.x, target.y);
        let (mut pace, mut still) = (Pace::default(), 0);
        for _ in 0..MOVES {
            let cursor = cursor_now();
            let step = step_toward(cursor, target, pace);
            if step == (0, 0) {
                return Ok(());
            }
            self.send(&move_command(step))?;
            sleep(MOVED);
            let now = cursor_now();
            let went = (now.0 - cursor.0, now.1 - cursor.1);
            still = if went == (0, 0) { still + 1 } else { 0 };
            if still > STILL_MOVES {
                break;
            }
            pace = pace.after(step, went);
        }
        Err(MouseError::Failed(
            "the cursor did not reach the point".into(),
        ))
    }

    /// Press, drag or turn, with the cursor already on `from` (`to` is where a drag ends).
    fn perform(&self, motion: Motion, from: POINT, to: POINT) -> Result<(), MouseError> {
        match motion {
            Motion::Wheel { notches } => self.send(&wheel_command(notches)),
            Motion::Click { button, count } => {
                for press in 0..count {
                    if press > 0 {
                        sleep(PRESS);
                    }
                    self.send(&buttons_command(button.mask()))?;
                    sleep(PRESS);
                    self.send(&buttons_command(0))?;
                }
                Ok(())
            }
            Motion::Drag { button, .. } => {
                self.send(&buttons_command(button.mask()))?;
                sleep(PRESS);
                // The button is let go wherever the cursor got to.
                let moved = glide((from.x, from.y), (to.x, to.y), DRAG_STEPS)
                    .into_iter()
                    .try_for_each(|(x, y)| {
                        let there = self.put_cursor(POINT { x, y });
                        sleep(DRAG_STEP);
                        there
                    });
                sleep(PRESS);
                self.send(&buttons_command(0)).and(moved)
            }
        }
    }
}

/// Where the cursor is.
fn cursor_now() -> (i32, i32) {
    let mut point = POINT::default();
    // SAFETY: `point` is a live local the call writes the cursor position into.
    let _ = unsafe { GetCursorPos(&raw mut point) };
    (point.x, point.y)
}

/// Whether `window` is at the front, waiting a moment for it: Windows brings a window
/// forward shortly after it says it will (Blue Archive was not there yet when looked at
/// at once, measured on 2026-11-18).
fn came_to_front(window: HWND) -> bool {
    let deadline = Instant::now() + FRONT_WAIT;
    loop {
        // SAFETY: GetForegroundWindow takes no arguments and only returns a handle, which is
        // compared.
        if unsafe { GetForegroundWindow() } == window {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(FRONT_LOOK);
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
    fn act(&self, gesture: &Gesture) -> Result<(), MouseError> {
        // Without a board nothing is brought forward or moved.
        if !self.has_board() {
            return Err(MouseError::NoMouse);
        }
        let program = Program::Exe(gesture.process.clone());
        if !self.launcher.focus(&program).map_err(MouseError::Failed)? {
            return Err(MouseError::NoWindow);
        }
        let window = main_window_of(&gesture.process).ok_or(MouseError::NoWindow)?;
        if !came_to_front(window) {
            return Err(MouseError::Failed(format!(
                "{} did not come to the front",
                gesture.process
            )));
        }
        let mut client = RECT::default();
        // SAFETY: `window` is a window handle that is only read, and `client` is a live local the
        // call fills in.
        unsafe { GetClientRect(window, &raw mut client) }
            .map_err(|err| MouseError::Failed(err.to_string()))?;
        let on_screen = |x: f64, y: f64| {
            let (x, y) = in_client((client.right, client.bottom), x, y);
            let mut point = POINT { x, y };
            // SAFETY: `window` is a window handle that is only read, and `point` is a live local
            // the call converts in place.
            let _ = unsafe { ClientToScreen(window, &raw mut point) };
            point
        };
        let from = on_screen(gesture.x, gesture.y);
        let to = match gesture.motion {
            Motion::Drag { to_x, to_y, .. } => on_screen(to_x, to_y),
            _ => from,
        };
        // SAFETY: WindowFromPoint takes a point by value, and GetAncestor only reads the handle it
        // returned.
        let shows =
            |point: POINT| unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) } == window;
        if !shows(from) || !shows(to) {
            return Err(MouseError::Covered);
        }

        // The mouse acts on whatever is under the cursor, so the cursor visits the
        // point and then goes back where the user left it. The touch cursor keeper
        // would take the board for the user's mouse, so it waits meanwhile.
        let keeper_waits = board_at_work();
        let (x, y) = cursor_now();
        let saved = POINT { x, y };
        let result = self.put_cursor(from).and_then(|()| {
            sleep(ARRIVED);
            let done = self.perform(gesture.motion, from, to);
            sleep(SETTLE);
            done
        });
        let _ = self.put_cursor(saved);
        drop(keeper_waits);
        result
    }
}
