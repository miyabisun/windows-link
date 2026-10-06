//! Downloading purchased games and their updates into a shop's games folder: which
//! works to fetch, fetching a file so a stopped download goes on, unpacking it, where
//! it goes, and how an update is laid over the folder a game already has without
//! touching its saves.

use std::{
    collections::HashMap,
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use super::Work;

/// A folder name for a maker or title, as DLsiteNest names them: characters Windows
/// does not allow become `_` and dots are left out.
pub fn folder_name(text: &str) -> String {
    let name: String = text
        .chars()
        .filter(|c| *c != '.' && !c.is_control())
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    match name.trim() {
        "" => "_".to_owned(),
        name => name.to_owned(),
    }
}

/// A game on disk: its folder, the version windows-link put there (if it did), and
/// when the folder last changed.
#[derive(Clone, Debug, PartialEq)]
pub struct OnDisk {
    pub folder: PathBuf,
    pub version: Option<String>,
    pub modified: SystemTime,
}

/// What to do about the purchased games.
#[derive(Debug, Default, PartialEq)]
pub struct Plan<'a> {
    /// Games without a folder, to download.
    pub fresh: Vec<&'a Work>,
    /// Games whose folder is older than their latest version, to update in place.
    pub updates: Vec<(&'a Work, PathBuf)>,
    /// Games whose folder is the latest version though windows-link did not put it
    /// there: the version to remember for them.
    pub current: Vec<(&'a Work, String)>,
}

/// Plan the downloads of the games bought. A folder windows-link filled is current
/// while its version is the latest; any other folder is current when it changed after
/// the latest version came out (`released` reads the shop's dates; DLsiteNest does not
/// update games by itself).
#[allow(clippy::implicit_hasher, reason = "built by the library")]
pub fn plan<'a>(
    purchases: &'a [Work],
    on_disk: &HashMap<String, OnDisk>,
    released: &dyn Fn(&str) -> Option<SystemTime>,
) -> Plan<'a> {
    let mut plan = Plan::default();
    for work in purchases {
        let Some(disk) = on_disk.get(&work.id) else {
            plan.fresh.push(work);
            continue;
        };
        let current = if let Some(version) = &disk.version {
            *version >= work.version
        } else {
            let current = released(&work.version).is_none_or(|latest| disk.modified >= latest);
            if current {
                plan.current.push((work, work.version.clone()));
            }
            current
        };
        if !current {
            plan.updates.push((work, disk.folder.clone()));
        }
    }
    plan
}

/// Whether a file of a game is the player's own, which an update must not replace:
/// anything under a folder or named with `save` in it.
pub fn is_save(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .contains("save")
    })
}

/// The folder whose contents are the game in what an archive unpacked into `out`.
/// DLsite's archives wrap the game in a folder named after the work; that folder is
/// left out unless the game's folder on disk has one of the same name.
pub fn content_root(out: &Path, target: &Path) -> PathBuf {
    let entries: Vec<_> = std::fs::read_dir(out)
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    if let [only] = entries.as_slice()
        && only.file_type().is_ok_and(|kind| kind.is_dir())
        && !target.join(only.file_name()).exists()
    {
        return only.path();
    }
    out.to_path_buf()
}

/// Put an unpacked game in place. A new game's folder is moved to `target`. An update
/// moves each file over the game's folder: files the folder lacks are added, files it
/// has are replaced except saves, and files the update lacks stay.
pub fn place(root: &Path, target: &Path) -> std::io::Result<()> {
    if !target.exists() {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return std::fs::rename(root, target);
    }
    merge(root, target, Path::new(""))?;
    // What is left is the saves an update did not replace.
    std::fs::remove_dir_all(root)
}

/// Move the files under `from` over `to`; `at` is where `from` is in the game.
fn merge(from: &Path, to: &Path, at: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = at.join(entry.file_name());
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            merge(&entry.path(), &dest, &path)?;
        } else if !(is_save(&path) && dest.exists()) {
            std::fs::rename(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// The start of a RAR archive (versions 4 and 5).
const RAR_MARK: &[u8] = b"Rar!\x1a\x07";
/// How far into a self-extracting program its archive starts, at most.
const SELF_EXTRACTING_STUB: u64 = 4 << 20;

/// Whether `head` (the start of a file) holds a RAR archive's mark.
pub fn has_rar_mark(head: &[u8]) -> bool {
    head.windows(RAR_MARK.len()).any(|bytes| bytes == RAR_MARK)
}

/// Whether the file is a RAR: a `.rar`, or a self-extracting `.exe` with a RAR inside
/// (a self-extracting ZIP is not one).
fn is_rar(first: &Path) -> bool {
    let extension = first
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    match extension.as_deref() {
        Some("rar") => true,
        Some("exe") => {
            let mut head = Vec::new();
            std::fs::File::open(first)
                .and_then(|file| file.take(SELF_EXTRACTING_STUB).read_to_end(&mut head))
                .is_ok()
                && has_rar_mark(&head)
        }
        _ => false,
    }
}

/// Unpack the archive whose first file is `first` into `out`. A RAR goes through
/// `UnRAR`, all its parts: DLsite splits big works into `.part1.exe`, a self-extracting
/// first part, and `.part<N>.rar`; FANZA into `<name>.exe` and `<name>.r00` on. Anything
/// else (a ZIP, also a self-extracting one) goes through Windows' own `tar.exe`
/// (libarchive), without a console window. Names in a ZIP without the UTF-8 mark are
/// read as UTF-8 (DLsite's own archives write them so), else as CP932 (older Japanese
/// archives). (`tar.exe` reads only the first part of a split RAR and says nothing.)
pub fn unpack(first: &Path, out: &Path) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::fs::create_dir_all(out).map_err(|err| format!("{}: {err}", out.display()))?;
    if is_rar(first) {
        return unpack_rar(first, out);
    }
    let windows =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let mut said = String::new();
    for charset in ["UTF-8", "CP932"] {
        // Start over in an empty folder: a failed try skips the names it cannot read.
        let _ = std::fs::remove_dir_all(out);
        std::fs::create_dir_all(out).map_err(|err| format!("{}: {err}", out.display()))?;
        let output = Command::new(windows.join(r"System32\tar.exe"))
            .args(["--options", &format!("hdrcharset={charset}"), "-xf"])
            .arg(first)
            .arg("-C")
            .arg(out)
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|err| format!("tar: {err}"))?;
        if output.status.success() {
            return Ok(());
        }
        String::from_utf8_lossy(&output.stderr)
            .trim()
            .clone_into(&mut said);
        if !said.contains("cannot be converted") {
            break;
        }
    }
    Err(format!("cannot unpack {}: {said}", first.display()))
}

/// Unpack a RAR, going through all its parts. `UnRAR` finds the next parts from the
/// first one's name, which must then end in `.rar`: a self-extracting first part is
/// opened through a hard link of that name.
fn unpack_rar(first: &Path, out: &Path) -> Result<(), String> {
    let rar = first.with_extension("rar");
    let linked = rar != first;
    if linked {
        let _ = std::fs::remove_file(&rar);
        std::fs::hard_link(first, &rar).map_err(|err| format!("{}: {err}", rar.display()))?;
    }
    let unpacked = (|| -> unrar::UnrarResult<()> {
        let mut archive = unrar::Archive::new(&rar).open_for_processing()?;
        while let Some(header) = archive.read_header()? {
            archive = if header.entry().is_file() {
                header.extract_with_base(out)?
            } else {
                header.skip()?
            };
        }
        Ok(())
    })();
    if linked {
        let _ = std::fs::remove_file(&rar);
    }
    unpacked.map_err(|err| format!("cannot unpack {}: {err}", first.display()))
}

/// How long one file may take to download; a stalled one is cut and continued later.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_hours(6);
/// How often a download reports its progress.
const PROGRESS_STEP: u64 = 16 * 1024 * 1024;

/// The whole size from a `Content-Range` header such as `bytes 0-9/1234`.
pub fn content_total(range: &str) -> Option<u64> {
    range
        .strip_prefix("bytes ")?
        .rsplit('/')
        .next()?
        .parse()
        .ok()
}

/// Download `url` (the file `name`) to `path`, going on from a part already there.
/// `progress` hears the bytes on disk and the whole size now and then.
pub fn fetch(
    agent: &ureq::Agent,
    url: &str,
    name: &str,
    path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let failed = |err: &dyn std::fmt::Display| format!("{name}: {err}");
    let have = std::fs::metadata(path).map_or(0, |meta| meta.len());
    let request = agent.get(url);
    let request = if have > 0 {
        request.header("Range", format!("bytes={have}-"))
    } else {
        request
    };
    let mut response = match request
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()
    {
        Ok(response) => response,
        // Nothing is left after the part already there.
        Err(ureq::Error::StatusCode(416)) if have > 0 => return Ok(()),
        Err(err) => return Err(failed(&err)),
    };
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    if header("content-type").is_some_and(|kind| kind.contains("html")) {
        return Err(failed(&"the shop answered with a page instead of the file"));
    }
    let resumed = response.status() == 206;
    let start = if resumed { have } else { 0 };
    let total = header("content-range")
        .and_then(|range| content_total(&range))
        .or_else(|| {
            header("content-length")
                .and_then(|length| length.parse::<u64>().ok())
                .map(|length| start + length)
        })
        .ok_or_else(|| failed(&"the shop did not say how big it is"))?;
    let mut out = if resumed {
        std::fs::OpenOptions::new().append(true).open(path)
    } else {
        std::fs::File::create(path)
    }
    .map_err(|err| failed(&err))?;
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut buffer = vec![0; 1 << 20];
    let mut written = start;
    let mut reported = start;
    loop {
        let read = reader.read(&mut buffer).map_err(|err| failed(&err))?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read]).map_err(|err| failed(&err))?;
        written += read as u64;
        if written - reported >= PROGRESS_STEP {
            progress(written, total);
            reported = written;
        }
    }
    progress(written, total);
    if written == total {
        Ok(())
    } else {
        Err(failed(&format!("got {written} of {total} bytes")))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{content_root, content_total, folder_name, has_rar_mark, is_save, place};

    #[test]
    fn rar_archives_are_told_by_their_mark_even_behind_a_self_extracting_program() {
        assert!(has_rar_mark(b"Rar!\x1a\x07\x00\xcf\x90"));
        let behind = |mark: &[u8]| {
            let mut file = b"MZ\x90\x00".to_vec();
            file.extend(vec![0; 4096]);
            file.extend_from_slice(mark);
            file
        };
        // RAR 5 inside a self-extracting first part, and a self-extracting ZIP.
        assert!(has_rar_mark(&behind(b"Rar!\x1a\x07\x01\x00")));
        assert!(!has_rar_mark(&behind(b"PK\x03\x04")));
        assert!(!has_rar_mark(b"Rar!"));
    }

    #[test]
    fn reads_the_whole_size_from_a_content_range() {
        assert_eq!(content_total("bytes 0-0/478276006"), Some(478_276_006));
        assert_eq!(content_total("bytes */123"), Some(123));
        assert_eq!(content_total("bytes 0-9/*"), None);
        assert_eq!(content_total("nonsense"), None);
    }

    #[test]
    fn folder_names_follow_dlsitenest() {
        assert_eq!(folder_name("メスブタ/ゲスブタ"), "メスブタ_ゲスブタ");
        assert_eq!(
            folder_name("LOOK.hacII -ルック・ハックII-"),
            "LOOKhacII -ルック・ハックII-"
        );
        assert_eq!(folder_name("あの夏の島?"), "あの夏の島_");
        assert_eq!(folder_name("出会い電車:始発駅"), "出会い電車_始発駅");
        assert_eq!(folder_name(r#"a\b*c"d<e>f|g"#), "a_b_c_d_e_f_g");
        assert_eq!(folder_name(" 名前. "), "名前");
        assert_eq!(folder_name("..."), "_");
    }

    #[test]
    fn saves_are_told_apart() {
        assert!(is_save(Path::new("www/save/file1.rpgsave")));
        assert!(is_save(Path::new("Save01.rvdata2")));
        assert!(is_save(Path::new("SaveData/slot.dat")));
        assert!(!is_save(Path::new("img/title.png")));
        assert!(!is_save(Path::new("Game.exe")));
    }

    /// A fresh temporary folder for a test.
    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "windows-link-download-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn the_works_wrapper_folder_is_left_out_unless_the_game_has_it() {
        let dir = temp("root");
        let out = dir.join("out");
        write(&out.join("RJ1/Game.exe"), "");
        assert_eq!(content_root(&out, &dir.join("new")), out.join("RJ1"));
        write(&dir.join("old/RJ1/Game.exe"), "");
        assert_eq!(content_root(&out, &dir.join("old")), out);
        write(&out.join("readme.txt"), "");
        assert_eq!(content_root(&out, &dir.join("new")), out);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_game_moves_in_and_an_update_keeps_saves_and_extra_files() {
        let dir = temp("place");
        let fresh = dir.join("fresh");
        write(&fresh.join("Game.exe"), "v1");
        write(&fresh.join("data/map.dat"), "v1");
        let target = dir.join("Maker/Title");
        place(&fresh, &target).unwrap();
        assert_eq!(read(&target.join("Game.exe")).as_deref(), Some("v1"));
        assert_eq!(read(&target.join("data/map.dat")).as_deref(), Some("v1"));
        assert!(!fresh.exists());

        // The player has saved and the game wrote a file of its own.
        write(&target.join("save/file1.rpgsave"), "mine");
        write(&target.join("config.ini"), "mine");
        let update = dir.join("update");
        write(&update.join("Game.exe"), "v2");
        write(&update.join("data/new.dat"), "v2");
        write(&update.join("save/file1.rpgsave"), "shipped");
        write(&update.join("save/file2.rpgsave"), "shipped");
        place(&update, &target).unwrap();
        assert_eq!(read(&target.join("Game.exe")).as_deref(), Some("v2"));
        assert_eq!(read(&target.join("data/new.dat")).as_deref(), Some("v2"));
        assert_eq!(read(&target.join("data/map.dat")).as_deref(), Some("v1"));
        assert_eq!(read(&target.join("config.ini")).as_deref(), Some("mine"));
        assert_eq!(
            read(&target.join("save/file1.rpgsave")).as_deref(),
            Some("mine")
        );
        assert_eq!(
            read(&target.join("save/file2.rpgsave")).as_deref(),
            Some("shipped")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
