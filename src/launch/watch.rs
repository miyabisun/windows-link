//! Hears from Windows when a program may have started or stopped, so the buttons need
//! not be read again and again to find out:
//!
//! - a top-level window came or went (the shell's own notice, as the taskbar hears it):
//!   a program that started shows one, and one that was closed loses its own;
//! - a process the buttons show as running exited, also one without a window (Steam or
//!   Discord closed from the tray).
//!
//! Windows has no notice of every process starting that a program without administrator
//! rights can take, so a program that starts without showing a window is found the next
//! time anything has the buttons read.

use std::{
    sync::{Arc, Mutex, OnceLock, PoisonError, atomic::AtomicU32, atomic::Ordering},
    thread,
};

use tokio::sync::Notify;
use tracing::warn;
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{
                CreateEventW, GetProcessTimes, INFINITE, OpenProcess,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SetEvent,
                WaitForMultipleObjects,
            },
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HSHELL_WINDOWCREATED,
            HSHELL_WINDOWDESTROYED, KillTimer, MSG, RegisterClassW, RegisterShellHookWindow,
            RegisterWindowMessageW, SetTimer, TranslateMessage, WM_TIMER, WNDCLASSW,
            WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::w,
};

use super::{ProcessWatch, Processes, windows::snapshot};

/// A window that went may leave its process a moment more: the buttons are read once
/// more this long after.
const AFTER_CLOSE_MS: u32 = 2000;
/// As many processes as one wait takes, less the one for `wake`.
const MOST_WATCHED: usize = 63;

/// Whom the window procedure tells, and the shell's message (registered at start).
static CHANGED: OnceLock<Arc<Notify>> = OnceLock::new();
static SHELL_MESSAGE: AtomicU32 = AtomicU32::new(0);

/// A handle a thread owns and may hand to another.
struct Owned(HANDLE);
// SAFETY: an event or process handle is usable from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; the calls made on it (`SetEvent`, waits) are thread-safe.
unsafe impl Sync for Owned {}

pub struct WindowsWatch {
    /// The programs whose exit is waited for.
    wanted: Arc<Mutex<Processes>>,
    /// Set when `wanted` changed.
    wake: Arc<Owned>,
}

impl WindowsWatch {
    /// Start hearing; `changed` is notified each time a program may have started or
    /// stopped.
    pub fn start(changed: Arc<Notify>) -> Self {
        let wanted = Arc::new(Mutex::new(Processes::new()));
        // SAFETY: an unnamed auto-reset event with default security; no pointers are kept.
        let wake = Arc::new(Owned(
            unsafe { CreateEventW(None, false, false, None) }.unwrap_or_default(),
        ));
        let _ = CHANGED.set(changed.clone());
        if let Err(err) = thread::Builder::new()
            .name("window-watch".into())
            .spawn(windows)
        {
            warn!(%err, "cannot hear of windows coming and going");
        }
        let (programs, woken) = (wanted.clone(), wake.clone());
        if let Err(err) = thread::Builder::new()
            .name("exit-watch".into())
            .spawn(move || exits(&programs, &woken, &changed))
        {
            warn!(%err, "cannot hear of programs exiting");
        }
        Self { wanted, wake }
    }
}

impl ProcessWatch for WindowsWatch {
    fn watch_exits(&self, programs: Processes) {
        let mut wanted = self.wanted.lock().unwrap_or_else(PoisonError::into_inner);
        if *wanted != programs {
            *wanted = programs;
            // SAFETY: `wake` is the event made at start, never closed.
            let _ = unsafe { SetEvent(self.wake.0) };
        }
    }
}

unsafe extern "system" fn shell_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let changed = || {
        if let Some(changed) = CHANGED.get() {
            changed.notify_one();
        }
    };
    if message == SHELL_MESSAGE.load(Ordering::Relaxed) && message != 0 {
        let what = u32::try_from(wparam.0).unwrap_or(0);
        if what == HSHELL_WINDOWCREATED || what == HSHELL_WINDOWDESTROYED {
            changed();
        }
        if what == HSHELL_WINDOWDESTROYED {
            // SAFETY: `window` is this thread's own window; the timer has no callback, so it
            // posts WM_TIMER here (setting it again only moves its time).
            unsafe { SetTimer(Some(window), 1, AFTER_CLOSE_MS, None) };
        }
        return LRESULT(0);
    }
    if message == WM_TIMER {
        // SAFETY: timer 1 is the one set above on this window.
        let _ = unsafe { KillTimer(Some(window), 1) };
        changed();
        return LRESULT(0);
    }
    // SAFETY: the arguments are the ones Windows passed to this window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// A hidden window the shell tells of top-level windows coming and going, and its
/// message loop.
fn windows() {
    let class = w!("windows-link-shell");
    // SAFETY: the class names a static string and a window procedure of the right signature;
    // the window is created and used on this thread alone, and `msg` is a live local.
    unsafe {
        let instance = GetModuleHandleW(None).ok().map(Into::into);
        let hidden = WNDCLASSW {
            lpfnWndProc: Some(shell_proc),
            hInstance: instance.unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&raw const hidden) == 0 {
            warn!("cannot register the shell window class");
            return;
        }
        SHELL_MESSAGE.store(RegisterWindowMessageW(w!("SHELLHOOK")), Ordering::Relaxed);
        // Never shown: a tool window, so it is no taskbar window itself.
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("windows-link"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        );
        let Ok(window) = window else {
            warn!("cannot create the shell window");
            return;
        };
        if !RegisterShellHookWindow(window).as_bool() {
            warn!("cannot hear of windows coming and going");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

/// When the process `handle` is of was created, to tell a program's first process.
fn created(handle: HANDLE) -> Option<u64> {
    let mut times = [FILETIME::default(); 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: `handle` is a live process handle with query access, and the four are live locals
    // the call writes into.
    unsafe { GetProcessTimes(handle, creation, exit, kernel, user) }.ok()?;
    Some((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

/// A handle to wait on for each of `programs` that runs: its oldest process, which lasts
/// as long as the program (the others are its helpers, which come and go). The
/// processes in `gone` exited already and are left out, should Windows still list one.
fn oldest(programs: &Processes, gone: &[u32]) -> Vec<(u32, Owned)> {
    let mut found: std::collections::HashMap<String, (u64, u32, Owned)> =
        std::collections::HashMap::new();
    if programs.is_empty() {
        return Vec::new();
    }
    for (pid, name) in snapshot() {
        if !programs.contains(&name) || gone.contains(&pid) {
            continue;
        }
        // SAFETY: the handle is used only after OpenProcess succeeded and closed exactly once,
        // here or by whoever takes it from `found`.
        let Ok(handle) = (unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        }) else {
            continue;
        };
        let Some(at) = created(handle) else {
            // SAFETY: `handle` is the one just opened.
            let _ = unsafe { CloseHandle(handle) };
            continue;
        };
        let older = found.get(&name).is_none_or(|(known, _, _)| at < *known);
        let dropped = if older {
            found
                .insert(name, (at, pid, Owned(handle)))
                .map(|(_, _, old)| old)
        } else {
            Some(Owned(handle))
        };
        if let Some(Owned(handle)) = dropped {
            // SAFETY: `handle` is one opened above that nothing else keeps.
            let _ = unsafe { CloseHandle(handle) };
        }
    }
    found
        .into_values()
        .map(|(_, pid, handle)| (pid, handle))
        .take(MOST_WATCHED)
        .collect()
}

/// Wait for one of the wanted programs to exit and tell `changed`, starting over each
/// time the wanted programs change.
fn exits(wanted: &Mutex<Processes>, wake: &Owned, changed: &Notify) {
    // The processes seen to exit, so one Windows goes on listing is not waited on again
    // (its handle is signalled at once, which would turn the loop).
    let mut gone: Vec<u32> = Vec::new();
    loop {
        let programs = wanted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let watched = oldest(&programs, &gone);
        let handles: Vec<HANDLE> = std::iter::once(wake.0)
            .chain(watched.iter().map(|(_, handle)| handle.0))
            .collect();
        // SAFETY: every handle is live for the wait: `wake` for good, the processes' until
        // closed below.
        let signalled = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        let index = signalled.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        let exited = index
            .checked_sub(1)
            .and_then(|at| watched.get(at))
            .map(|(pid, _)| *pid);
        for (_, Owned(handle)) in watched {
            // SAFETY: `handle` is one `oldest` opened, closed exactly once here.
            let _ = unsafe { CloseHandle(handle) };
        }
        if index == 0 {
            // The wanted programs changed.
            continue;
        }
        if let Some(pid) = exited {
            // Process IDs come round again: only the latest exits are remembered.
            if gone.len() >= MOST_WATCHED {
                gone.remove(0);
            }
            gone.push(pid);
            changed.notify_one();
            continue;
        }
        // The wait itself failed: do not spin.
        warn!("waiting for programs to exit failed; their exit shows when the buttons are read");
        return;
    }
}
