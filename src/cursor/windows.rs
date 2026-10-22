//! Putting the highlighted arrow in for Windows' arrow, and Windows' own back.

use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, time::Duration};

use tracing::warn;
use windows::{
    Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                GetCursorPos, HCURSOR, IMAGE_CURSOR, LR_LOADFROMFILE, LoadImageW, OCR_NORMAL,
                SPI_SETCURSORS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetSystemCursor,
                SystemParametersInfoW,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::Palette;
use crate::{
    config::ButtonSpec,
    config::Config,
    hold::{Colors, Holds},
};

/// How often the buttons, and the monitor the cursor is on, are looked at.
const POLL: Duration = Duration::from_millis(150);

/// The IDs of the `cursor.highlight` buttons of `config`.
pub fn buttons(config: &Config) -> Vec<String> {
    config
        .buttons
        .iter()
        .filter(|b| matches!(b.spec, ButtonSpec::CursorHighlight {}))
        .map(|b| b.id.clone())
        .collect()
}

/// Keep the cursor highlighted while one of `buttons` is on in `highlights`, in the
/// colour chosen for it in `colors` (the first's, should more be on), at the size for
/// the monitor the cursor is on, writing the cursor files into `dir`. Runs for good
/// on its own thread.
pub fn run(buttons: &[String], highlights: &Holds, colors: &Colors, dir: &Path) {
    // A run stopped while it showed the highlight leaves it until Windows' arrow is put back.
    restore();
    let mut shown: Option<(u32, Palette)> = None;
    loop {
        std::thread::sleep(POLL);
        let on = highlights.held().unwrap_or_default();
        if let Some(id) = buttons.iter().find(|id| on.contains(*id)) {
            let palette = colors
                .all()
                .unwrap_or_default()
                .get(id)
                .copied()
                .unwrap_or_default();
            let wanted = (size_here(), palette);
            if shown != Some(wanted) {
                if let Err(err) = apply(dir, wanted.0, palette) {
                    warn!(%err, "cannot highlight the cursor");
                }
                shown = Some(wanted);
            }
        } else if shown.take().is_some() {
            restore();
        }
    }
}

/// Windows' own cursors back, as the user's scheme has them.
pub fn restore() {
    // SAFETY: SPI_SETCURSORS reloads the system cursors and takes no buffer, so passing no pointer
    // is valid.
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

/// The arrow in `palette`, `size` pixels square, in for Windows' arrow. Its file is
/// written afresh each time: one an older version wrote under the same name draws
/// that version's arrow (v0.1.28 left its 1.5x one behind).
fn apply(dir: &Path, size: u32, palette: Palette) -> Result<(), String> {
    let path = dir.join(format!("cursor-highlight-{}-{size}.cur", palette.name()));
    let file = super::cur(&super::picture(size, palette), size, super::hotspot(size));
    std::fs::write(&path, file).map_err(|e| e.to_string())?;
    let cursor = load(&path, size)?;
    // SAFETY: `cursor` is a cursor `load` just created and nothing else owns; SetSystemCursor takes
    // it over.
    unsafe { SetSystemCursor(cursor, OCR_NORMAL) }.map_err(|e| e.to_string())
}

fn load(path: &Path, size: u32) -> Result<HCURSOR, String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let pixels = i32::try_from(size).map_err(|e| e.to_string())?;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, and with
    // LR_LOADFROMFILE no module handle is needed.
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

/// As big as Windows draws its arrow on the monitor the cursor is on: the pointer size
/// the user chose (`CursorBaseSize`, 32 by default) at that monitor's DPI.
fn size_here() -> u32 {
    let mut base = 0u32;
    let mut len = u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4);
    // SAFETY: `base` and `len` are live locals; `len` holds the byte size of `base`, which is all a
    // REG_DWORD value can write.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Control Panel\Cursors"),
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
    let size = base.min(256) as f32 * dpi_here().min(960) as f32 / 96.0;
    super::to_byte_range(size, 255.0).max(32)
}

/// The DPI of the monitor the cursor is on (96 when it cannot be read).
fn dpi_here() -> u32 {
    let mut point = POINT::default();
    // SAFETY: `point` is a live local the call writes the cursor position into.
    if unsafe { GetCursorPos(&raw mut point) }.is_err() {
        return 96;
    }
    // SAFETY: MonitorFromPoint only reads the point; with MONITOR_DEFAULTTONEAREST it always
    // returns a monitor.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: `monitor` came from MonitorFromPoint, and `x` and `y` are live locals the call writes
    // into.
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut x, &raw mut y) } {
        Ok(()) if x > 0 => x,
        _ => 96,
    }
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
    // SAFETY: `cursor` is the live cursor `load` returned, and `info` is a live local the call
    // fills in.
    unsafe { GetIconInfo(HICON(cursor.0), &raw mut info) }.ok()?;
    let mut bitmap = BITMAP::default();
    // SAFETY: `hbmColor` is the bitmap GetIconInfo returned, and the size passed is that of
    // `bitmap`, the live local written into.
    let read = unsafe {
        GetObjectW(
            info.hbmColor.into(),
            i32::try_from(std::mem::size_of::<BITMAP>()).unwrap_or(0),
            Some((&raw mut bitmap).cast::<c_void>()),
        )
    };
    // SAFETY: the two bitmaps GetIconInfo created and the cursor `load` created belong to this
    // function alone and are freed exactly once, after their last use.
    unsafe {
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        let _ = DestroyCursor(cursor);
    }
    (read > 0).then(|| u32::try_from(bitmap.bmWidth).unwrap_or(0))
}
