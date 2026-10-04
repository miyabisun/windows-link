//! Starting programs the way Explorer does, and finding or closing running ones.

use std::collections::HashSet;

/// Which executables are running, by lower-case file name.
pub type Processes = HashSet<String>;

pub trait Launcher: Send + Sync + 'static {
    /// Open an exe, shortcut, document or URL (for example `steam://rungameid/…`).
    fn open(&self, target: &str, args: Option<&str>) -> Result<(), String>;
    fn processes(&self) -> Processes;
    /// Ask the process's windows to close, like clicking their close button. Returns
    /// how many windows were asked.
    fn close(&self, process: &str) -> Result<usize, String>;
}

pub fn is_running(processes: &Processes, process: &str) -> bool {
    processes.contains(&process.to_lowercase())
}

/// Where the target's own folder is, for programs that expect to start there.
pub fn working_dir(target: &str) -> Option<std::path::PathBuf> {
    if target.contains("://") {
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
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
                    SW_SHOWNORMAL, WM_CLOSE,
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

    pub struct WindowsLauncher;

    impl Launcher for WindowsLauncher {
        fn open(&self, target: &str, args: Option<&str>) -> Result<(), String> {
            let file = wide(OsStr::new(target));
            let params = args.map(|a| wide(OsStr::new(a)));
            let dir = working_dir(target).map(|d| wide(d.as_os_str()));
            let as_ptr =
                |v: &Option<Vec<u16>>| v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
            let result = unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
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
            let wanted = process.to_lowercase();
            let mut search = Search {
                pids: snapshot()
                    .into_iter()
                    .filter(|(_, name)| *name == wanted)
                    .map(|(pid, _)| pid)
                    .collect(),
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
    }

    impl Launcher for FakeLauncher {
        fn open(&self, target: &str, args: Option<&str>) -> Result<(), String> {
            self.opened
                .lock()
                .unwrap()
                .push(args.map_or_else(|| target.to_owned(), |a| format!("{target} {a}")));
            Ok(())
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
        assert_eq!(working_dir("notepad.exe"), None);
    }

    #[test]
    fn process_names_match_without_regard_to_case() {
        let running: Processes = ["streetfighter6.exe".to_owned()].into();
        assert!(is_running(&running, "StreetFighter6.exe"));
        assert!(!is_running(&running, "BlueArchive.exe"));
    }
}
