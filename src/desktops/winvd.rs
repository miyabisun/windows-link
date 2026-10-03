//! Virtual desktop access through the `winvd` crate (undocumented Windows COM
//! interfaces). Kept to this one file so a Windows update that breaks it is fixed here.

use std::{
    ffi::c_void,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

use tracing::{info, warn};
use windows058::{Win32::Foundation::HWND, core::GUID};

use super::{DesktopError, DesktopInfo, VirtualDesktops, display_name, reason};

/// How often the watcher checks that Explorer's virtual desktop service still answers.
const HEALTH_CHECK: Duration = Duration::from_secs(3);

fn guid(id: &GUID) -> String {
    format!("{id:?}")
}

fn failed(context: &str, error: &::winvd::Error) -> String {
    format!("{context}: {error:?}")
}

pub struct WinvdDesktops;

impl VirtualDesktops for WinvdDesktops {
    fn list(&self) -> Result<Vec<DesktopInfo>, String> {
        let current = ::winvd::get_current_desktop()
            .and_then(|d| d.get_id())
            .map_err(|e| failed("current desktop", &e))?;
        let desktops = ::winvd::get_desktops().map_err(|e| failed("desktops", &e))?;
        desktops
            .iter()
            .map(|desktop| {
                let id = desktop.get_id().map_err(|e| failed("desktop id", &e))?;
                let index = desktop
                    .get_index()
                    .map_err(|e| failed("desktop index", &e))?;
                let name = desktop.get_name().unwrap_or_default();
                Ok(DesktopInfo {
                    id: guid(&id),
                    name: display_name(&name, index),
                    index,
                    current: id == current,
                })
            })
            .collect()
    }

    fn switch(&self, id: &str) -> Result<(), DesktopError> {
        let desktops =
            ::winvd::get_desktops().map_err(|e| DesktopError::Failed(failed("desktops", &e)))?;
        let target = desktops
            .iter()
            .find(|d| d.get_id().is_ok_and(|g| guid(&g).eq_ignore_ascii_case(id)))
            .ok_or(DesktopError::NotFound)?;
        let index = target
            .get_index()
            .map_err(|e| DesktopError::Failed(failed("desktop index", &e)))?;
        ::winvd::switch_desktop(index).map_err(|e| DesktopError::Failed(failed("switch", &e)))
    }

    fn pin_window(&self, hwnd: isize) -> Result<(), DesktopError> {
        let handle = HWND(hwnd as *mut c_void);
        ::winvd::pin_window(handle).map_err(|e| match e {
            ::winvd::Error::WindowNotFound => DesktopError::NotFound,
            other => DesktopError::Failed(failed("pin", &other)),
        })
    }
}

/// Report desktop list changes to `on_change` (with the event name) until the process
/// exits. When Explorer restarts the listener stops working; the watcher notices on its
/// health check, waits for the service to come back, re-subscribes and reports
/// `"reconnected"`.
pub fn watch(on_change: impl Fn(&'static str) + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("desktop-watch".into())
        .spawn(move || {
            let mut broken = false;
            loop {
                let (tx, rx) = mpsc::channel::<::winvd::DesktopEvent>();
                let listener = match ::winvd::listen_desktop_events(tx) {
                    Ok(listener) => listener,
                    Err(err) => {
                        if !broken {
                            warn!(error = ?err, "cannot listen to virtual desktop events");
                            broken = true;
                        }
                        thread::sleep(HEALTH_CHECK);
                        continue;
                    }
                };
                // `broken` is set again below before the inner loop can end.
                if broken {
                    info!("virtual desktop service is back; listening again");
                    on_change("reconnected");
                }
                loop {
                    match rx.recv_timeout(HEALTH_CHECK) {
                        Ok(event) => {
                            if let Some(name) = reason(&event) {
                                on_change(name);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if ::winvd::get_current_desktop().is_err() {
                                warn!("virtual desktop service stopped answering");
                                broken = true;
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            broken = true;
                            break;
                        }
                    }
                }
                drop(listener);
                thread::sleep(HEALTH_CHECK);
            }
        });
    if let Err(err) = spawned {
        warn!(%err, "cannot start the virtual desktop watcher");
    }
}
