//! Putting the rainbow arrow in for Windows' arrow, and Windows' own back.

use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, time::Duration};

use tracing::warn;
use windows::{
    Win32::{
        Foundation::HWND,
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        UI::{
            HiDpi::GetDpiForWindow,
            WindowsAndMessaging::{
                HCURSOR, IMAGE_CURSOR, LR_LOADFROMFILE, LoadImageW, OCR_NORMAL, SPI_SETCURSORS,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetSystemCursor, SystemParametersInfoW,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{config::ButtonSpec, config::Config, hold::Holds, launch::windows::front_window_of};

/// How often the window in front is looked at.
const POLL: Duration = Duration::from_millis(150);
/// How many times Windows' own arrow the rainbow one is.
const SCALE: f32 = 1.5;

/// A `cursor.highlight` button: its ID and the program it highlights over.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub button: String,
    pub process: String,
}

/// The `cursor.highlight` buttons of `config`.
pub fn targets(config: &Config) -> Vec<Target> {
    config
        .buttons
        .iter()
        .filter_map(|b| match &b.spec {
            ButtonSpec::CursorHighlight { process } => Some(Target {
                button: b.id.clone(),
                process: process.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Keep the cursor highlighted while a button in `highlights` is on and its program is
/// in front, writing the cursor files into `dir`. Runs for good on its own thread.
pub fn run(targets: &[Target], highlights: &Holds, dir: &Path) {
    // A run stopped while it showed the rainbow leaves it until Windows' arrow is put back.
    restore();
    let mut shown: Option<u32> = None;
    loop {
        std::thread::sleep(POLL);
        let on = highlights.held().unwrap_or_default();
        let front = targets
            .iter()
            .filter(|t| on.contains(&t.button))
            .find_map(|t| front_window_of(&t.process));
        match front {
            Some(window) => {
                let size = size_for(window);
                if shown != Some(size) {
                    match apply(dir, size) {
                        Ok(()) => shown = Some(size),
                        Err(err) => {
                            warn!(%err, "cannot highlight the cursor");
                            shown = Some(size);
                        }
                    }
                }
            }
            None if shown.is_some() => {
                restore();
                shown = None;
            }
            None => {}
        }
    }
}

/// Windows' own cursors back, as the user's scheme has them.
pub fn restore() {
    if let Err(err) = unsafe {
        SystemParametersInfoW(
            SPI_SETCURSORS,
            0,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    } {
        warn!(%err, "cannot put Windows' cursors back");
    }
}

/// The rainbow arrow, `size` pixels square, in for Windows' arrow.
fn apply(dir: &Path, size: u32) -> Result<(), String> {
    let path = dir.join(format!("cursor-highlight-{size}.ani"));
    if !path.exists() {
        let file = super::ani(&super::frames(size), size, super::hotspot(size));
        std::fs::write(&path, file).map_err(|e| e.to_string())?;
    }
    let cursor = load(&path, size)?;
    unsafe { SetSystemCursor(cursor, OCR_NORMAL) }.map_err(|e| e.to_string())
}

fn load(path: &Path, size: u32) -> Result<HCURSOR, String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let pixels = i32::try_from(size).map_err(|e| e.to_string())?;
    let handle = unsafe {
        LoadImageW(
            None,
            PCWSTR(wide.as_ptr()),
            IMAGE_CURSOR,
            pixels,
            pixels,
            LR_LOADFROMFILE,
        )
    }
    .map_err(|e| e.to_string())?;
    Ok(HCURSOR(handle.0))
}

/// Half again as big as Windows draws its arrow over `window`: the pointer size the
/// user chose (`CursorBaseSize`, 32 by default) at the window's DPI.
fn size_for(window: HWND) -> u32 {
    let mut base = 0u32;
    let mut len = u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4);
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Cursors"),
            w!("CursorBaseSize"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut base).cast::<c_void>()),
            Some(&raw mut len),
        )
    };
    if read.is_err() || base == 0 {
        base = 32;
    }
    let dpi = match unsafe { GetDpiForWindow(window) } {
        0 => 96,
        dpi => dpi,
    };
    let size = base.min(256) as f32 * dpi.min(960) as f32 / 96.0 * SCALE;
    super::to_byte_range(size, 255.0).max(32)
}

/// The size Windows loads the cursor file at `path` in, asked for `size` (for tests).
#[cfg(test)]
pub fn loaded_size(path: &Path, size: u32) -> Option<u32> {
    use windows::Win32::{
        Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW},
        UI::WindowsAndMessaging::{DestroyCursor, GetIconInfo, HICON, ICONINFO},
    };
    let cursor = load(path, size).ok()?;
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(HICON(cursor.0), &raw mut info) }.ok()?;
    let mut bitmap = BITMAP::default();
    let read = unsafe {
        GetObjectW(
            info.hbmColor.into(),
            i32::try_from(std::mem::size_of::<BITMAP>()).unwrap_or(0),
            Some((&raw mut bitmap).cast::<c_void>()),
        )
    };
    unsafe {
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        let _ = DestroyCursor(cursor);
    }
    (read > 0).then(|| u32::try_from(bitmap.bmWidth).unwrap_or(0))
}
