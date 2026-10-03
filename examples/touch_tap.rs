//! Development tool: inject synthetic touch input to verify the touch cursor keeper
//! without a finger. Not part of the server.
//!
//!   `cargo run --example touch_tap -- tap <x> <y>`
//!   `cargo run --example touch_tap -- swipe <x1> <y1> <x2> <y2>`
//!   `cargo run --example touch_tap -- park <x> <y>`  (put the mouse there as a real mouse would)
//!
//! Coordinates are physical screen pixels. The cursor position is printed before,
//! right after, and 300 ms after the gesture.

use std::{mem, process::ExitCode, thread, time::Duration};

use windows::Win32::{
    Foundation::{POINT, RECT},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
        },
        Input::Pointer::{
            InitializeTouchInjection, InjectTouchInput, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT,
            POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS,
            POINTER_TOUCH_INFO, TOUCH_FEEDBACK_DEFAULT,
        },
        WindowsAndMessaging::{GetCursorPos, PT_TOUCH, SetCursorPos},
    },
};

const TOUCH_MASK_CONTACTAREA: u32 = 0x1;
const TOUCH_MASK_ORIENTATION: u32 = 0x2;
const TOUCH_MASK_PRESSURE: u32 = 0x4;

fn cursor() -> (i32, i32) {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&raw mut point) };
    (point.x, point.y)
}

fn contact(x: i32, y: i32, flags: POINTER_FLAGS) -> POINTER_TOUCH_INFO {
    let mut info: POINTER_TOUCH_INFO = unsafe { mem::zeroed() };
    info.pointerInfo.pointerType = PT_TOUCH;
    info.pointerInfo.ptPixelLocation = POINT { x, y };
    info.pointerInfo.pointerFlags = flags;
    info.touchMask = TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION | TOUCH_MASK_PRESSURE;
    info.rcContact = RECT {
        left: x - 2,
        top: y - 2,
        right: x + 2,
        bottom: y + 2,
    };
    info.orientation = 90;
    info.pressure = 32000;
    info
}

fn inject(info: &POINTER_TOUCH_INFO) -> Result<(), String> {
    unsafe { InjectTouchInput(std::slice::from_ref(info)) }.map_err(|e| e.to_string())
}

fn down() -> POINTER_FLAGS {
    POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
}

fn moving() -> POINTER_FLAGS {
    POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
}

fn run(args: &[i32], swipe: bool) -> Result<(), String> {
    unsafe { InitializeTouchInjection(1, TOUCH_FEEDBACK_DEFAULT) }.map_err(|e| e.to_string())?;
    println!("cursor before     = {:?}", cursor());
    let (x1, y1) = (args[0], args[1]);
    inject(&contact(x1, y1, down()))?;
    if swipe {
        let (x2, y2) = (args[2], args[3]);
        for step in 1..=20 {
            thread::sleep(Duration::from_millis(16));
            let x = x1 + (x2 - x1) * step / 20;
            let y = y1 + (y2 - y1) * step / 20;
            inject(&contact(x, y, moving()))?;
        }
        thread::sleep(Duration::from_millis(16));
        inject(&contact(x2, y2, POINTER_FLAG_UP))?;
    } else {
        thread::sleep(Duration::from_millis(80));
        inject(&contact(x1, y1, POINTER_FLAG_UP))?;
    }
    println!("cursor right after = {:?}", cursor());
    thread::sleep(Duration::from_millis(300));
    println!("cursor 300ms after = {:?}", cursor());
    Ok(())
}

/// Move the cursor, then nudge it 1 px and back with real mouse input so hooks
/// record a real-mouse position there (zero-length moves are dropped by Windows).
fn park(x: i32, y: i32) -> Result<(), String> {
    unsafe { SetCursorPos(x, y) }.map_err(|e| e.to_string())?;
    let nudge = |dx: i32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dwFlags: MOUSEEVENTF_MOVE,
                ..Default::default()
            },
        },
    };
    let size = i32::try_from(mem::size_of::<INPUT>()).unwrap_or_default();
    if unsafe { SendInput(&[nudge(1), nudge(-1)], size) } != 2 {
        return Err("SendInput failed".to_owned());
    }
    thread::sleep(Duration::from_millis(50));
    println!("parked at {:?}", cursor());
    Ok(())
}

fn main() -> ExitCode {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let numbers: Option<Vec<i32>> = args.iter().skip(1).map(|a| a.parse().ok()).collect();
    let result = match (args.first().map(String::as_str), numbers) {
        (Some("tap"), Some(n)) if n.len() == 2 => run(&n, false),
        (Some("swipe"), Some(n)) if n.len() == 4 => run(&n, true),
        (Some("park"), Some(n)) if n.len() == 2 => park(n[0], n[1]),
        _ => Err(
            "usage: touch_tap tap <x> <y> | swipe <x1> <y1> <x2> <y2> | park <x> <y>".to_owned(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
