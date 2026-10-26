//! Windows side of resting: brightness over DDC/CI (VCP 10), and black windows laid
//! over monitors by a thread of their own.

use std::{mem, sync::mpsc, thread};

use tracing::{error, warn};
use windows::{
    Win32::{
        Devices::Display::{
            DestroyPhysicalMonitors, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR,
            SetVCPFeature,
        },
        Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BLACK_BRUSH, EnumDisplayMonitors, GetMonitorInfoW, GetStockObject, HBRUSH, HDC,
            HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
            PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW, SW_SHOWNOACTIVATE,
            ShowWindow, TranslateMessage, WM_APP, WM_USER, WNDCLASSW, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::{BOOL, w},
};

use super::Screens;
use crate::touch::{Monitor, windows::wide};

/// The MCCS code of a monitor's brightness.
const BRIGHTNESS: u8 = 0x10;
/// Posted to the cover thread when there are new monitors to cover.
const COVER: u32 = WM_APP + 1;

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the address of the `Vec` that `find` passes to EnumDisplayMonitors,
    // which calls back synchronously while that `Vec` is borrowed for nothing else.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(monitor);
    BOOL::from(true)
}

/// The monitor whose GDI name is `gdi_name`, and the rectangle it shows.
fn find(gdi_name: &str) -> Option<(HMONITOR, RECT)> {
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: `collect_monitor` reads `data` as the `Vec` given here, which outlives the
    // synchronous call.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(std::ptr::from_mut(&mut monitors) as isize),
        )
        .ok()
        .ok()?;
    }
    monitors.into_iter().find_map(|monitor| {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize =
            u32::try_from(mem::size_of::<MONITORINFOEXW>()).unwrap_or_default();
        // SAFETY: `info` is a live MONITORINFOEXW whose cbSize says so, which GetMonitorInfoW
        // accepts through a MONITORINFO pointer.
        let found = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
        (found.as_bool() && wide(&info.szDevice).eq_ignore_ascii_case(gdi_name))
            .then_some((monitor, info.monitorInfo.rcMonitor))
    })
}

/// Run `talk` with the DDC/CI handle of the monitor named `gdi_name`.
fn with_ddc<T>(gdi_name: &str, talk: impl FnOnce(HANDLE) -> Option<T>) -> Option<T> {
    let (monitor, _) = find(gdi_name)?;
    let mut count = 0u32;
    // SAFETY: `count` is a live local the call writes into.
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &raw mut count) }.ok()?;
    if count == 0 {
        return None;
    }
    let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
    // SAFETY: `physical` holds exactly the `count` entries the call fills.
    unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut physical) }.ok()?;
    let result = talk(physical[0].hPhysicalMonitor);
    // SAFETY: the handles came from GetPhysicalMonitorsFromHMONITOR and are not used again.
    let _ = unsafe { DestroyPhysicalMonitors(&physical) };
    result
}

pub struct WindowsScreens {
    /// The cover thread's id and the monitors' rectangles to cover next.
    cover: Option<(u32, mpsc::Sender<Vec<RECT>>)>,
}

impl WindowsScreens {
    /// Start the thread that owns the black windows. Without it, resting only darkens.
    pub fn start() -> Self {
        let (rects_tx, rects_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("rest-cover".into())
            .spawn(move || cover_thread(&rects_rx, &ready_tx));
        let cover = match spawned.map_err(|e| e.to_string()).and_then(|_| {
            ready_rx
                .recv()
                .map_err(|_| "the cover thread exited during start".to_owned())?
        }) {
            Ok(thread) => Some((thread, rects_tx)),
            Err(message) => {
                error!(%message, "black windows are unavailable; resting only darkens");
                None
            }
        };
        Self { cover }
    }
}

impl Screens for WindowsScreens {
    fn brightness(&self, monitor: &Monitor) -> Option<u32> {
        with_ddc(&monitor.gdi_name, |handle| {
            let mut current = 0u32;
            // SAFETY: `handle` is a live physical monitor handle and `current` a live local.
            let read = unsafe {
                GetVCPFeatureAndVCPFeatureReply(handle, BRIGHTNESS, None, &raw mut current, None)
            };
            (read != 0).then_some(current)
        })
    }

    fn set_brightness(&self, monitor: &Monitor, value: u32) -> Result<(), String> {
        with_ddc(&monitor.gdi_name, |handle| {
            // SAFETY: `handle` is a live physical monitor handle.
            (unsafe { SetVCPFeature(handle, BRIGHTNESS, value) } != 0).then_some(())
        })
        .ok_or_else(|| {
            format!(
                "{} did not take brightness {value} over DDC/CI",
                monitor.name.trim()
            )
        })
    }

    fn cover(&self, monitors: &[Monitor]) {
        let Some((thread, next)) = &self.cover else {
            return;
        };
        let rects: Vec<RECT> = monitors
            .iter()
            .filter_map(|m| find(&m.gdi_name).map(|(_, rect)| rect))
            .collect();
        let sent = next.send(rects).is_ok();
        // SAFETY: posting a message with no pointers to a thread id that owns a queue.
        if !sent || unsafe { PostThreadMessageW(*thread, COVER, WPARAM(0), LPARAM(0)) }.is_err() {
            warn!("cannot reach the cover thread");
        }
    }
}

unsafe extern "system" fn cover_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the arguments are the ones Windows passed to this window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn cover_thread(rects: &mpsc::Receiver<Vec<RECT>>, ready: &mpsc::Sender<Result<u32, String>>) {
    let class = w!("windows-link-rest");
    // SAFETY: `msg` is a live local; the class names a static string and a window procedure of
    // the right signature; the windows are created, shown and destroyed on this thread alone.
    unsafe {
        let mut msg = MSG::default();
        // Make this thread's message queue before anyone posts to it.
        let _ = PeekMessageW(&raw mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        let instance = GetModuleHandleW(None).ok().map(Into::into);
        let black = WNDCLASSW {
            lpfnWndProc: Some(cover_proc),
            hInstance: instance.unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: class,
            ..Default::default()
        };
        let registered = RegisterClassW(&raw const black);
        if registered == 0 {
            let _ = ready.send(Err("cannot register the black window class".into()));
            return;
        }
        let _ = ready.send(Ok(GetCurrentThreadId()));
        let mut windows: Vec<HWND> = Vec::new();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            if msg.message != COVER {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
                continue;
            }
            let Some(latest) = rects.try_iter().last() else {
                continue;
            };
            for window in windows.drain(..) {
                let _ = DestroyWindow(window);
            }
            for rect in latest {
                // Not activated: the window under the panel's tap keeps the focus.
                match CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class,
                    w!("windows-link rest"),
                    WS_POPUP,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    None,
                    None,
                    instance,
                    None,
                ) {
                    Ok(window) => {
                        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
                        windows.push(window);
                    }
                    Err(err) => warn!(%err, "cannot lay a black window"),
                }
            }
        }
    }
}
