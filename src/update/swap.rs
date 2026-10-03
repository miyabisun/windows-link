//! Replacing the running exe: `<exe>.new` is written and checked, the running exe is
//! renamed to `<exe>.old`, `<exe>.new` takes its name, and the restarted process deletes
//! `<exe>.old` once it is listening.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// `CREATE_BREAKAWAY_FROM_JOB`: leave Task Scheduler's job so the new process outlives
/// this one even if the job would end with it.
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut path = exe.as_os_str().to_owned();
    path.push(suffix);
    path.into()
}

pub fn old_path(exe: &Path) -> PathBuf {
    sibling(exe, ".old")
}

/// Write the downloaded exe next to the running one and make sure it runs and reports
/// `version`. Returns the staged path.
pub fn stage(exe: &Path, bytes: &[u8], version: &str) -> Result<PathBuf, String> {
    let staged = sibling(exe, ".new");
    let write = || -> std::io::Result<()> {
        let mut file = File::create(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(err) = write() {
        let _ = fs::remove_file(&staged);
        return Err(format!("cannot write {}: {err}", staged.display()));
    }
    match reported_version(&staged) {
        Ok(printed) if printed == format!("windows-link {version}") => Ok(staged),
        Ok(printed) => {
            let _ = fs::remove_file(&staged);
            Err(format!(
                "downloaded exe reports `{printed}`, expected {version}"
            ))
        }
        Err(err) => {
            let _ = fs::remove_file(&staged);
            Err(format!("cannot run the downloaded exe: {err}"))
        }
    }
}

fn reported_version(exe: &Path) -> std::io::Result<String> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "--version did not finish",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut out)?;
    }
    Ok(out.trim().to_owned())
}

/// Rename the running exe to `<exe>.old` and move `staged` into its place. On failure
/// the running exe keeps its name.
pub fn replace(exe: &Path, staged: &Path) -> Result<(), String> {
    let old = old_path(exe);
    match fs::remove_file(&old) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            let _ = fs::remove_file(staged);
            return Err(format!("cannot remove {}: {err}", old.display()));
        }
    }
    if let Err(err) = fs::rename(exe, &old) {
        let _ = fs::remove_file(staged);
        return Err(format!("cannot rename {}: {err}", exe.display()));
    }
    if let Err(err) = fs::rename(staged, exe) {
        let _ = fs::rename(&old, exe);
        let _ = fs::remove_file(staged);
        return Err(format!("cannot move the new exe into place: {err}"));
    }
    Ok(())
}

/// Undo [`replace`]: put `<exe>.old` back.
pub fn rollback(exe: &Path) -> std::io::Result<()> {
    let old = old_path(exe);
    fs::remove_file(exe)?;
    fs::rename(old, exe)
}

/// Start `exe` with this process's arguments; environment and working directory are
/// inherited.
pub fn relaunch(exe: &Path) -> std::io::Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let spawn = |flags: u32| {
        Command::new(exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // A job that forbids breaking away refuses the flag; start inside it then.
    spawn(CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(0))
        .map(drop)
}

/// Delete `<exe>.old` left by the update that started this process. The previous
/// process may take a moment to exit, so retry for a while in the background.
pub fn remove_old_later(exe: &Path) {
    let old = old_path(exe);
    if !old.exists() {
        return;
    }
    thread::spawn(move || {
        for _ in 0..40 {
            match fs::remove_file(&old) {
                Ok(()) => {
                    info!(path = %old.display(), "removed the previous version");
                    return;
                }
                Err(err) if err.kind() == ErrorKind::NotFound => return,
                Err(_) => thread::sleep(Duration::from_millis(250)),
            }
        }
        warn!(path = %old.display(), "cannot remove the previous version; the next update will");
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{old_path, replace, rollback};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("windows-link-swap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_keeps_the_previous_exe_as_old_and_rollback_restores_it() {
        let dir = temp_dir("replace");
        let exe = dir.join("windows-link.exe");
        let staged = dir.join("windows-link.exe.new");
        fs::write(&exe, "v1").unwrap();
        fs::write(old_path(&exe), "v0 left over").unwrap();
        fs::write(&staged, "v2").unwrap();

        replace(&exe, &staged).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert_eq!(fs::read_to_string(old_path(&exe)).unwrap(), "v1");
        assert!(!staged.exists());

        rollback(&exe).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_path(&exe).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_replace_leaves_the_running_exe_in_place() {
        let dir = temp_dir("missing");
        let exe = dir.join("windows-link.exe");
        fs::write(&exe, "v1").unwrap();
        let missing = dir.join("windows-link.exe.new");

        assert!(replace(&exe, &missing).is_err());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_path(&exe).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
