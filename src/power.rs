//! Putting the PC to sleep.

/// Sleep now, as the Start menu's Sleep does. Blocking until the PC wakes up again.
pub fn sleep() -> Result<(), String> {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, LUID},
            Security::{
                AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
                SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
            },
            System::{
                Power::SetSuspendState,
                Threading::{GetCurrentProcess, OpenProcessToken},
            },
        },
        core::w,
    };

    unsafe {
        // Sleeping needs the shutdown privilege, which users have but is off by default.
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        )
        .map_err(|err| format!("cannot open the process token: {err}"))?;
        let mut luid = LUID::default();
        let enabled = LookupPrivilegeValueW(None, w!("SeShutdownPrivilege"), &raw mut luid)
            .and_then(|()| {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                AdjustTokenPrivileges(token, false, Some(&raw const privileges), 0, None, None)
            });
        let _ = CloseHandle(token);
        enabled.map_err(|err| format!("cannot enable the shutdown privilege: {err}"))?;
        if SetSuspendState(false, false, false) {
            Ok(())
        } else {
            Err(format!(
                "Windows refused to sleep: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
}
