//! Windows side of the touch cursor keeper: monitor enumeration (`DisplayConfig` and
//! pointer devices) and the low-level mouse hook thread.

use std::{
    collections::{HashMap, HashSet},
    mem,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use windows::{
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
            DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
            GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
        },
        Foundation::{LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONULL,
            MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
        },
        System::{LibraryLoader::GetModuleHandleW, SystemInformation::GetTickCount64},
        UI::{
            Controls::{POINTER_DEVICE_INFO, POINTER_DEVICE_TYPE_TOUCH},
            Input::Pointer::GetPointerDevices,
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, MONITORINFOF_PRIMARY,
                MSG, MSLLHOOKSTRUCT, SetCursorPos, SetTimer, SetWindowsHookExW, TranslateMessage,
                WH_MOUSE_LL, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER,
            },
        },
    },
    core::BOOL,
};

use super::{Displays, KeepCursor, Monitor, Observation, Source, classify, is_stray, monitor_id};

/// Cursor poll period; a tap is restored within a few polls of the finger lifting.
const POLL_MS: u32 = 40;

static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static LAST_REAL_MS: AtomicU64 = AtomicU64::new(0);
static TOUCH_DOWN: AtomicBool = AtomicBool::new(false);

fn wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// GDI name (`\\.\DISPLAYn`) -> (friendly name, device path) for active displays.
fn display_paths() -> HashMap<String, (String, String)> {
    let mut found = HashMap::new();
    unsafe {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut path_count,
            &raw mut mode_count,
        )
        .is_err()
        {
            return found;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut path_count,
            paths.as_mut_ptr(),
            &raw mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return found;
        }
        for path in &paths[..path_count as usize] {
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = u32::try_from(mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>())
                .unwrap_or_default();
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = u32::try_from(mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>())
                .unwrap_or_default();
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&raw mut target.header) != 0
                || DisplayConfigGetDeviceInfo(&raw mut source.header) != 0
            {
                continue;
            }
            found.insert(
                wide(&source.viewGdiDeviceName),
                (
                    wide(&target.monitorFriendlyDeviceName),
                    wide(&target.monitorDevicePath),
                ),
            );
        }
    }
    found
}

fn gdi_name(monitor: HMONITOR) -> Option<(String, bool)> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = u32::try_from(mem::size_of::<MONITORINFOEXW>()).unwrap_or_default();
    unsafe {
        GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>())
            .as_bool()
            .then(|| {
                (
                    wide(&info.szDevice),
                    info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                )
            })
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<(String, bool)>) };
    if let Some(entry) = gdi_name(monitor) {
        list.push(entry);
    }
    BOOL::from(true)
}

/// GDI names of monitors that a touch digitizer is mapped to.
fn touch_gdi_names() -> HashSet<String> {
    let mut names = HashSet::new();
    unsafe {
        let mut count = 0u32;
        if GetPointerDevices(&raw mut count, None).is_err() || count == 0 {
            return names;
        }
        let mut devices = vec![POINTER_DEVICE_INFO::default(); count as usize];
        if GetPointerDevices(&raw mut count, Some(devices.as_mut_ptr())).is_err() {
            return names;
        }
        for device in &devices[..count as usize] {
            if device.pointerDeviceType == POINTER_DEVICE_TYPE_TOUCH
                && let Some((name, _)) = gdi_name(device.monitor)
            {
                names.insert(name);
            }
        }
    }
    names
}

pub struct WindowsDisplays;

impl Displays for WindowsDisplays {
    fn monitors(&self) -> Result<Vec<Monitor>, String> {
        let mut gdi: Vec<(String, bool)> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(collect_monitor),
                LPARAM(std::ptr::from_mut(&mut gdi) as isize),
            )
            .ok()
            .map_err(|e| format!("EnumDisplayMonitors: {e}"))?;
        }
        let paths = display_paths();
        let touch = touch_gdi_names();
        Ok(gdi
            .into_iter()
            .map(|(gdi_name, primary)| {
                let (name, device_path) = paths
                    .get(&gdi_name)
                    .cloned()
                    .unwrap_or_else(|| (gdi_name.clone(), gdi_name.clone()));
                Monitor {
                    id: monitor_id(&device_path),
                    name,
                    touch: touch.contains(&gdi_name),
                    device_path,
                    gdi_name,
                    primary,
                }
            })
            .collect())
    }
}

/// Monitor under a screen point (physical pixels): its ID and whether a touch
/// digitizer is mapped to it.
fn monitor_at(x: i32, y: i32) -> Option<MonitorAt> {
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) };
    if monitor.is_invalid() {
        return None;
    }
    let (gdi, _) = gdi_name(monitor)?;
    let (_, path) = display_paths().remove(&gdi)?;
    Some((monitor_id(&path), touch_gdi_names().contains(&gdi)))
}

fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        match classify(info.dwExtraInfo) {
            Source::Mouse => {
                LAST_X.store(info.pt.x, Ordering::Relaxed);
                LAST_Y.store(info.pt.y, Ordering::Relaxed);
                LAST_REAL_MS.store(now_ms(), Ordering::Relaxed);
            }
            Source::Touch => {
                let up = matches!(u32::try_from(wparam.0), Ok(WM_LBUTTONUP | WM_RBUTTONUP));
                TOUCH_DOWN.store(!up, Ordering::Relaxed);
            }
            Source::Pen => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Install the hook and the cursor poller on a dedicated thread. The process must
/// already be per-monitor DPI aware so hook, cursor and monitor coordinates agree.
pub fn start_hook(keep: KeepCursor) -> Result<(), String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("touch-hook".into())
        .spawn(move || hook_thread(&keep, &ready_tx))
        .map_err(|e| format!("cannot start touch hook thread: {e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "touch hook thread exited during start".to_owned())?
}

fn hook_thread(keep: &KeepCursor, ready: &mpsc::Sender<Result<(), String>>) {
    unsafe {
        let mut start = POINT::default();
        if GetCursorPos(&raw mut start).is_ok() {
            LAST_X.store(start.x, Ordering::Relaxed);
            LAST_Y.store(start.y, Ordering::Relaxed);
        }
        LAST_REAL_MS.store(now_ms(), Ordering::Relaxed);
        let module = GetModuleHandleW(None).ok();
        let installed = SetWindowsHookExW(WH_MOUSE_LL, Some(hook), module.map(Into::into), 0);
        if let Err(err) = installed {
            let _ = ready.send(Err(format!("SetWindowsHookExW: {err}")));
            return;
        }
        SetTimer(None, 0, POLL_MS, None);
        let _ = ready.send(Ok(()));

        let mut poller = Poller::default();
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER {
                poller.tick(keep);
            } else {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }
}

/// Monitor ID and whether a touch digitizer is mapped to it.
type MonitorAt = (String, bool);

#[derive(Default)]
struct Poller {
    previous: Option<(i32, i32)>,
    /// Monitor lookup for the last stray position (the lookup is the expensive part;
    /// the keep-cursor setting is re-read every tick so switching it on takes effect).
    looked_up: Option<((i32, i32), Option<MonitorAt>)>,
}

impl Poller {
    fn tick(&mut self, keep: &KeepCursor) {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&raw mut point) }.is_err() {
            return;
        }
        let cursor = (point.x, point.y);
        let real = (
            LAST_X.load(Ordering::Relaxed),
            LAST_Y.load(Ordering::Relaxed),
        );
        let observation = Observation {
            cursor,
            previous: self.previous,
            real,
            since_real_ms: now_ms().saturating_sub(LAST_REAL_MS.load(Ordering::Relaxed)),
            touch_down: TOUCH_DOWN.load(Ordering::Relaxed),
        };
        self.previous = Some(cursor);
        if !is_stray(&observation) {
            return;
        }
        let monitor = match &self.looked_up {
            Some((at, monitor)) if *at == cursor => monitor.clone(),
            _ => {
                let monitor = monitor_at(cursor.0, cursor.1);
                self.looked_up = Some((cursor, monitor.clone()));
                monitor
            }
        };
        let Some((id, true)) = monitor else {
            return;
        };
        if !keep.get(&id) {
            return;
        }
        let _ = unsafe { SetCursorPos(real.0, real.1) };
        // If Windows clipped the target, where it landed is the real position.
        let mut landed = POINT::default();
        if unsafe { GetCursorPos(&raw mut landed) }.is_ok() {
            LAST_X.store(landed.x, Ordering::Relaxed);
            LAST_Y.store(landed.y, Ordering::Relaxed);
            self.previous = Some((landed.x, landed.y));
        }
        tracing::debug!(?cursor, ?real, monitor = %id, "cursor restored after touch");
    }
}
