//! Starting programs the way Explorer does, and finding, focusing or closing running ones.

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

/// Which executables are running, by lower-case file name.
pub type Processes = HashSet<String>;

/// Which processes belong to a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// Processes of this executable file name, case-insensitive.
    Exe(String),
    /// Processes whose executable is in this folder or below it, such as a game started
    /// through Steam, whose executable name the library does not know.
    Folder(PathBuf),
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exe(name) => f.write_str(name),
            Self::Folder(folder) => write!(f, "{}", folder.display()),
        }
    }
}

pub trait Launcher: Send + Sync + 'static {
    /// Open an exe, shortcut, document, URL (`steam://rungameid/…`) or Store app
    /// (`shell:AppsFolder\…`), as administrator when `admin`.
    fn open(&self, target: &str, args: Option<&str>, admin: bool) -> Result<(), String>;
    fn processes(&self) -> Processes;
    /// Bring the program's main window to the virtual desktop on screen and to the front
    /// (restoring it when minimized). `Ok(false)` when it has no window to show.
    fn focus(&self, program: &Program) -> Result<bool, String>;
    /// In the background, wait for the program's main window to appear and `focus` it
    /// once. A program started through another one (a game through Steam) otherwise
    /// starts behind the window that had the focus, and a game then may not go full
    /// screen.
    fn focus_when_ready(&self, program: Program);
    /// Ask the process's windows to close, like clicking their close button. Returns
    /// how many windows were asked.
    fn close(&self, process: &str) -> Result<usize, String>;
}

pub fn is_running(processes: &Processes, process: &str) -> bool {
    processes.contains(&process.to_lowercase())
}

/// Whether `exe` is in `folder` or below it, ignoring case as Windows does.
pub fn is_under(exe: &Path, folder: &Path) -> bool {
    let mut parts = exe.components();
    folder.components().all(|want| {
        parts.next().is_some_and(|part| {
            part.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&want.as_os_str().to_string_lossy())
        })
    }) && parts.next().is_some()
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
    use std::{
        collections::HashSet,
        ffi::OsStr,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::PathBuf,
    };

    use windows::{
        Win32::{
            Foundation::{CloseHandle, HWND, LPARAM, WPARAM},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                Threading::{
                    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                    QueryFullProcessImageNameW,
                },
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
        core::{BOOL, PCWSTR, PWSTR, w},
    };

    use super::{Launcher, Processes, Program, is_under, working_dir};

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

    /// The executable's full path, when Windows lets this user read it.
    fn exe_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = [0u16; 1024];
            let mut len = u32::try_from(buffer.len()).unwrap_or(0);
            let read = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &raw mut len,
            );
            let _ = CloseHandle(process);
            read.ok()?;
            let len = usize::try_from(len).unwrap_or(0).min(buffer.len());
            Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])))
        }
    }

    fn pids(program: &Program) -> HashSet<u32> {
        match program {
            Program::Exe(name) => pids_of(name),
            Program::Folder(folder) => snapshot()
                .into_iter()
                .filter(|(pid, _)| exe_path(*pid).is_some_and(|exe| is_under(&exe, folder)))
                .map(|(pid, _)| pid)
                .collect(),
        }
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

    /// How long a started program may take to show its window.
    const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

    pub struct WindowsLauncher;

    impl Launcher for WindowsLauncher {
        fn focus_when_ready(&self, program: Program) {
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + READY_TIMEOUT;
                while std::time::Instant::now() < deadline {
                    if main_window(&pids(&program)).is_some() {
                        match WindowsLauncher.focus(&program) {
                            Ok(_) => {
                                tracing::info!(%program, "brought the started program to the front");
                            }
                            Err(message) => {
                                tracing::warn!(%message, "cannot bring the started program to the front");
                            }
                        }
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                tracing::warn!(%program, "the started program showed no window in time");
            });
        }

        fn focus(&self, program: &Program) -> Result<bool, String> {
            let Some(hwnd) = main_window(&pids(program)) else {
                return Ok(false);
            };
            // Bringing forward a window on another virtual desktop would switch the
            // screen to that desktop; bring the window here instead.
            match crate::desktops::winvd::bring_to_current_desktop(hwnd.0 as isize) {
                Ok(true) => tracing::info!(%program, "moved the program to this virtual desktop"),
                Ok(false) => {}
                Err(message) => {
                    tracing::warn!(%message, "cannot move the program to this virtual desktop");
                }
            }
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
                    Err(format!("Windows did not bring {program} to the front"))
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
    use std::{path::PathBuf, sync::Mutex};

    use super::{Launcher, Processes, Program};

    #[derive(Default)]
    pub struct FakeLauncher {
        pub running: Mutex<Processes>,
        /// Folders a running program was started from.
        pub running_folders: Mutex<Vec<PathBuf>>,
        pub opened: Mutex<Vec<String>>,
        pub focused: Mutex<Vec<Program>>,
        pub awaited: Mutex<Vec<Program>>,
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

        fn focus_when_ready(&self, program: Program) {
            self.awaited.lock().unwrap().push(program);
        }

        fn focus(&self, program: &Program) -> Result<bool, String> {
            let running = match program {
                Program::Exe(name) => self.running.lock().unwrap().contains(&name.to_lowercase()),
                Program::Folder(folder) => self.running_folders.lock().unwrap().contains(folder),
            };
            if running {
                self.focused.lock().unwrap().push(program.clone());
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

    use std::path::Path;

    use super::{Processes, is_running, is_under, working_dir};

    #[test]
    fn a_program_is_under_its_folder_at_any_depth_ignoring_case() {
        let folder = Path::new(r"C:\Program Files (x86)\Steam\steamapps\common\Street Fighter 6");
        assert!(is_under(
            Path::new(
                r"c:\program files (x86)\steam\steamapps\common\street fighter 6\StreetFighter6.exe"
            ),
            folder
        ));
        assert!(is_under(
            Path::new(
                r"C:\Program Files (x86)\Steam\steamapps\common\Street Fighter 6\bin\x64\game.exe"
            ),
            folder
        ));
        assert!(!is_under(
            Path::new(
                r"C:\Program Files (x86)\Steam\steamapps\common\Street Fighter 6 Demo\game.exe"
            ),
            folder
        ));
        assert!(!is_under(folder, folder));
        assert!(!is_under(
            Path::new(r"C:\Program Files (x86)\Steam\steam.exe"),
            folder
        ));
    }

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
