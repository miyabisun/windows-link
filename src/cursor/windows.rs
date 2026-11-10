//! Putting the highlight's cursors in for Windows' (the arrow, the busy ones and the hand
//! over a link), and Windows' own back.

use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use tracing::warn;
use windows::{
    Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                GetCursorPos, HCURSOR, IMAGE_CURSOR, LR_LOADFROMFILE, LoadImageW, OCR_APPSTARTING,
                OCR_HAND, OCR_NORMAL, OCR_WAIT, SPI_SETCURSORS, SYSTEM_CURSOR_ID,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetSystemCursor, SystemParametersInfoW,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::{Arrow, Palette, art::Art};
use crate::{config::ButtonSpec, config::Config};

/// The IDs of the `cursor.highlight` buttons of `config`.
pub fn buttons(config: &Config) -> Vec<String> {
    config
        .buttons
        .iter()
        .filter(|b| matches!(b.spec, ButtonSpec::CursorHighlight {}))
        .map(|b| b.id.clone())
        .collect()
}

/// Windows' arrow, changed once each time a button is pressed or its colour chosen;
/// the cursor files are written into the folder given. Nothing watches the cursor: the
/// highlight is as big as Windows draws its arrow on the monitor the cursor is on when
/// it is put in, and stays so (the user's of 2026-11-10; until then a thread looked
/// every 150 ms for the monitor under the cursor).
pub struct WindowsArrow(pub PathBuf);

impl Arrow for WindowsArrow {
    fn show(&self, palette: Option<Palette>) {
        match palette {
            Some(palette) => {
                if let Err(err) = apply(&self.0, size_here(), palette) {
                    warn!(%err, "cannot highlight the cursor");
                }
            }
            None => restore(),
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

/// The highlight's cursors in `palette`, each put in once, their files written afresh
/// each time (one an older version wrote under the same name draws that version's
/// arrow: v0.1.28 left its 1.5x one behind):
///
/// - the arrow, `size` pixels square, for Windows' arrow;
/// - for the arrow Windows shows while a program starts, the same arrow with the
///   user's `busy` picture turning beside it, or the arrow alone without one: Windows'
///   own is smaller than the highlight, so the cursor shrank for a moment (the user's
///   of 2026-11-10);
/// - the `busy` picture alone, turning, for Windows' wait cursor;
/// - the user's `link-<colour>` picture for the hand over a link, pointing with its
///   middle (the user's of 2026-11-10: a halo to aim with). A colour without one
///   leaves the hand as it is.
fn apply(dir: &Path, size: u32, palette: Palette) -> Result<(), String> {
    let write = |what: &str, file: Vec<u8>| {
        let path = dir.join(format!("cursor-highlight-{what}"));
        std::fs::write(&path, file)
            .map(|()| path)
            .map_err(|e| e.to_string())
    };
    let colour = palette.name();
    let arrow = super::cur(&super::picture(size, palette), size, super::hotspot(size));
    let arrow = write(&format!("{colour}-{size}.cur"), arrow)?;
    put(&arrow, size, OCR_NORMAL)?;

    // A picture alone, `across` pixels square, pointing with its middle.
    let alone = |art: &Art, across: u32, turn: f32| {
        let (middle, half) = (across as f32 / 2.0, u16::try_from(across / 2).unwrap_or(0));
        let picture = art.drawn(across, (middle, middle), across as f32, turn);
        super::cur(&picture, across, (half, half))
    };
    if let Some(mark) = art(dir, "busy") {
        // A cursor is at most 256 pixels square, and this one wider than its arrow by its mark.
        let small = size.min(157);
        let width = super::busy_side(small);
        let frames: Vec<Vec<u8>> = super::busy_frames(small, palette, &mark)
            .iter()
            .map(|frame| super::cur(frame, width, super::hotspot(small)))
            .collect();
        let busy = super::ani(&frames, super::TURN_JIFFIES);
        put(
            &write(&format!("{colour}-busy-{size}.ani"), busy)?,
            width,
            OCR_APPSTARTING,
        )?;
        let frames: Vec<Vec<u8>> = super::turns()
            .map(|turn| alone(&mark, size, turn))
            .collect();
        let wait = super::ani(&frames, super::TURN_JIFFIES);
        put(&write(&format!("wait-{size}.ani"), wait)?, size, OCR_WAIT)?;
    } else {
        put(&arrow, size, OCR_APPSTARTING)?;
    }
    if let Some(aim) = art(dir, &format!("link-{colour}")) {
        // A quarter wider than the arrow: a ring to aim with reads thin at the arrow's size.
        let across = (size * 5 / 4).min(256);
        let link = write(
            &format!("link-{colour}-{size}.cur"),
            alone(&aim, across, 0.0),
        )?;
        put(&link, across, OCR_HAND)?;
    }
    Ok(())
}

/// The user's picture `cursor-<name>.png` in the data folder `dir`, if there is one that
/// can be read.
fn art(dir: &Path, name: &str) -> Option<Art> {
    let path = dir.join(format!("cursor-{name}.png"));
    let file = std::fs::read(&path).ok()?;
    Art::decode(&file)
        .inspect_err(|err| warn!(%err, path = %path.display(), "cannot read a cursor picture"))
        .ok()
}

/// The cursor file at `path`, `size` pixels square, in for Windows' cursor `id`.
fn put(path: &Path, size: u32, id: SYSTEM_CURSOR_ID) -> Result<(), String> {
    let cursor = load(path, size)?;
    // SAFETY: `cursor` is a cursor `load` just created and nothing else owns; SetSystemCursor takes
    // it over.
    unsafe { SetSystemCursor(cursor, id) }.map_err(|e| e.to_string())
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
