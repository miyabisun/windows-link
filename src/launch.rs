//! Starting programs the way Explorer does, and finding, focusing or closing running ones.

use std::collections::HashSet;

/// Which executables are running, by lower-case file name.
pub type Processes = HashSet<String>;

pub trait Launcher: Send + Sync + 'static {
    /// Open an exe, shortcut, document, URL (`steam://rungameid/…`) or Store app
    /// (`shell:AppsFolder\…`), as administrator when `admin`.
    fn open(&self, target: &str, args: Option<&str>, admin: bool) -> Result<(), String>;
    fn processes(&self) -> Processes;
    /// Bring the process's main window to the front (restoring it when minimized).
    /// `Ok(false)` when it has no window to show.
    fn focus(&self, process: &str) -> Result<bool, String>;
    /// Ask the process's windows to close, like clicking their close button. Returns
    /// how many windows were asked.
    fn close(&self, process: &str) -> Result<usize, String>;
}

pub fn is_running(processes: &Processes, process: &str) -> bool {
    processes.contains(&process.to_lowercase())
}

/// Where the target's own folder is, for programs that expect to start there. URLs,
/// shell locations and bare program names have none.
pub fn working_dir(target: &str) -> Option<std::path::PathBuf> {
    if target.contains("://") || target.to_lowercase().starts_with("shell:") {
        return None;
    }
    std::path::Path::new(target)
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
}

pub mod windows {
    use std::{collections::HashSet, ffi::OsStr, os::windows::ffi::OsStrExt};

    use windows::{
        Win32::{
            Foundation::{CloseHandle, HWND, LPARAM, WPARAM},
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            UI::{
                Input::KeyboardAndMouse::{
                    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                    SendInput, VK_MENU,
                },
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW,
                    GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
                    PostMessageW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
                    WM_CLOSE, WS_EX_TOOLWINDOW,
                },
            },
        },
        core::{BOOL, PCWSTR, w},
    };

    use super::{Launcher, Processes, working_dir};

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    fn snapshot() -> Vec<(u32, String)> {
        let mut found = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return found;
            };
            let mut entry = PROCESSENTRY32W {
                dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
                ..Default::default()
            };
            let mut ok = Process32FirstW(snap, &raw mut entry).is_ok();
            while ok {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                found.push((
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase(),
                ));
                ok = Process32NextW(snap, &raw mut entry).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        found
    }

    fn pids_of(process: &str) -> HashSet<u32> {
        let wanted = process.to_lowercase();
        snapshot()
            .into_iter()
            .filter(|(_, name)| *name == wanted)
            .map(|(pid, _)| pid)
            .collect()
    }

    /// The first visible, titled, unowned window of one of `pids` (the main window).
    fn main_window(pids: &HashSet<u32>) -> Option<HWND> {
        struct Search<'a> {
            pids: &'a HashSet<u32>,
            found: Option<HWND>,
        }
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let search = unsafe { &mut *(data.0 as *mut Search<'_>) };
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
            let tool =
                unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned() & WS_EX_TOOLWINDOW.0;
            let main = search.pids.contains(&pid)
                && unsafe { IsWindowVisible(hwnd) }.as_bool()
                && unsafe { GetWindow(hwnd, GW_OWNER) }.is_err()
                && unsafe { GetWindowTextLengthW(hwnd) } > 0
                && tool == 0;
            if main {
                search.found = Some(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut search = Search { pids, found: None };
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&raw mut search as isize));
        }
        search.found
    }

    pub struct WindowsLauncher;

    impl Launcher for WindowsLauncher {
        fn focus(&self, process: &str) -> Result<bool, String> {
            let pids = pids_of(process);
            let Some(hwnd) = main_window(&pids) else {
                return Ok(false);
            };
            let key = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                // Windows lets only the process that got the last input take the
                // foreground; a tap on the panel went elsewhere, so press Alt first.
                let alt = [key(KEYBD_EVENT_FLAGS::default()), key(KEYEVENTF_KEYUP)];
                SendInput(
                    &alt,
                    i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(0),
                );
                if SetForegroundWindow(hwnd).as_bool() {
                    Ok(true)
                } else {
                    Err(format!("Windows did not bring {process} to the front"))
                }
            }
        }

        fn open(&self, target: &str, args: Option<&str>, admin: bool) -> Result<(), String> {
            let file = wide(OsStr::new(target));
            let params = args.map(|a| wide(OsStr::new(a)));
            let dir = working_dir(target).map(|d| wide(d.as_os_str()));
            let as_ptr =
                |v: &Option<Vec<u16>>| v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
            let result = unsafe {
                ShellExecuteW(
                    None,
                    if admin { w!("runas") } else { w!("open") },
                    PCWSTR(file.as_ptr()),
                    as_ptr(&params),
                    as_ptr(&dir),
                    SW_SHOWNORMAL,
                )
            };
            // Values above 32 mean success.
            if result.0 as isize > 32 {
                Ok(())
            } else {
                Err(format!(
                    "cannot open {target} (ShellExecute {})",
                    result.0 as isize
                ))
            }
        }

        fn processes(&self) -> Processes {
            snapshot().into_iter().map(|(_, name)| name).collect()
        }

        fn close(&self, process: &str) -> Result<usize, String> {
            struct Search {
                pids: HashSet<u32>,
                asked: usize,
            }
            unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
                let search = unsafe { &mut *(data.0 as *mut Search) };
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
                if search.pids.contains(&pid)
                    && unsafe { IsWindowVisible(hwnd) }.as_bool()
                    && unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
                {
                    search.asked += 1;
                }
                BOOL(1)
            }
            let mut search = Search {
                pids: pids_of(process),
                asked: 0,
            };
            if search.pids.is_empty() {
                return Ok(0);
            }
            unsafe {
                EnumWindows(Some(each), LPARAM(&raw mut search as isize))
                    .map_err(|err| format!("cannot list windows: {err}"))?;
            }
            Ok(search.asked)
        }
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::{Launcher, Processes};

    #[derive(Default)]
    pub struct FakeLauncher {
        pub running: Mutex<Processes>,
        pub opened: Mutex<Vec<String>>,
        pub focused: Mutex<Vec<String>>,
    }

    impl Launcher for FakeLauncher {
        fn open(&self, target: &str, args: Option<&str>, admin: bool) -> Result<(), String> {
            let mut line = args.map_or_else(|| target.to_owned(), |a| format!("{target} {a}"));
            if admin {
                line.push_str(" (admin)");
            }
            self.opened.lock().unwrap().push(line);
            Ok(())
        }

        fn focus(&self, process: &str) -> Result<bool, String> {
            let running = self
                .running
                .lock()
                .unwrap()
                .contains(&process.to_lowercase());
            if running {
                self.focused.lock().unwrap().push(process.to_owned());
            }
            Ok(running)
        }

        fn processes(&self) -> Processes {
            self.running.lock().unwrap().clone()
        }

        fn close(&self, process: &str) -> Result<usize, String> {
            Ok(usize::from(
                self.running.lock().unwrap().remove(&process.to_lowercase()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Processes, is_running, working_dir};

    #[test]
    fn programs_start_in_their_own_folder_but_urls_do_not() {
        assert_eq!(
            working_dir(r"C:\YostarGames\Launcher\launcher.exe"),
            Some(PathBuf::from(r"C:\YostarGames\Launcher"))
        );
        assert_eq!(working_dir("steam://rungameid/1364780"), None);
        assert_eq!(
            working_dir(r"shell:AppsFolder\Claude_pzs8sxrjxfjjc!Claude"),
            None
        );
        assert_eq!(working_dir("notepad.exe"), None);
    }

    #[test]
    fn process_names_match_without_regard_to_case() {
        let running: Processes = ["streetfighter6.exe".to_owned()].into();
        assert!(is_running(&running, "StreetFighter6.exe"));
        assert!(!is_running(&running, "BlueArchive.exe"));
    }
}
