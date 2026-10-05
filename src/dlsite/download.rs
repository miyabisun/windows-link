//! Downloading purchased DLsite games and their updates into the games folder:
//! which works to fetch, where they go, and how an update is laid over the folder a
//! game already has without touching its saves.

use std::{
    collections::HashMap,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::play::Work;

/// DLsite's work types of games (action, quiz, adventure, RPG, table, digital novel,
/// simulation, typing, shooting, puzzle, other games).
const GAME_KINDS: [&str; 11] = [
    "ACN", "QIZ", "ADV", "RPG", "TBL", "DNV", "SLN", "TYP", "STG", "PZL", "ETC",
];

/// Whether a purchase is a game for this PC: a game that runs on Windows, and not the
/// AI-translated data of a game in another language that comes with some purchases.
pub fn is_game(work: &Work) -> bool {
    GAME_KINDS.contains(&work.kind.as_str())
        && work.windows
        && !work.name.contains("ゲームデータ（AI翻訳）")
}

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

/// A time DLsite gives, such as `2017-12-09T15:00:00.000000Z` (UTC).
pub fn parse_date(text: &str) -> Option<SystemTime> {
    let number = |at: std::ops::Range<usize>| text.get(at)?.parse::<i64>().ok();
    if text.get(4..5)? != "-" || text.get(10..11)? != "T" {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let seconds = number(11..13)? * 3600 + number(14..16)? * 60 + number(17..19)?;
    // Days since 1970-01-01, by Howard Hinnant's days_from_civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    let since = u64::try_from(days * 86_400 + seconds).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(since))
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

/// Plan the downloads. A folder windows-link filled is current while its version is
/// the latest; any other folder is current when it changed after the latest version
/// came out (DLsiteNest does not update games by itself).
#[allow(clippy::implicit_hasher, reason = "built by the library")]
pub fn plan<'a>(purchases: &'a [Work], on_disk: &HashMap<String, OnDisk>) -> Plan<'a> {
    let mut plan = Plan::default();
    for work in purchases.iter().filter(|work| is_game(work)) {
        let Some(disk) = on_disk.get(&work.id) else {
            plan.fresh.push(work);
            continue;
        };
        let current = if let Some(version) = &disk.version {
            *version >= work.version
        } else {
            let current = parse_date(&work.version).is_none_or(|latest| disk.modified >= latest);
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

/// Unpack the archive whose first file is `first` into `out`. A RAR (DLsite splits big
/// works into `.part1.exe`, a self-extracting first part, and `.part<N>.rar`) goes
/// through `UnRAR`. Anything else goes through Windows' own `tar.exe` (libarchive),
/// without a console window. Names in a ZIP without the UTF-8 mark are read as UTF-8
/// (DLsite's own archives write them so), else as CP932 (older Japanese archives).
/// (`tar.exe` reads only the first part of a split RAR and says nothing.)
pub fn unpack(first: &Path, out: &Path) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::fs::create_dir_all(out).map_err(|err| format!("{}: {err}", out.display()))?;
    let rar = first.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("rar") || extension.eq_ignore_ascii_case("exe")
    });
    if rar {
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

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        time::{Duration, UNIX_EPOCH},
    };

    use super::{
        OnDisk, Plan, content_root, folder_name, is_game, is_save, parse_date, place, plan,
    };
    use crate::dlsite::play::Work;

    fn work(id: &str, kind: &str, windows: bool, version: &str) -> Work {
        Work {
            id: id.into(),
            name: format!("{id} name"),
            maker: "Maker".into(),
            kind: kind.into(),
            windows,
            version: version.into(),
            ..Work::default()
        }
    }

    #[test]
    fn games_for_windows_are_downloaded_but_not_translations_or_phone_only_ones() {
        assert!(is_game(&work("RJ1", "RPG", true, "")));
        assert!(is_game(&work("RJ2", "SLN", true, "")));
        // A voice work, a game for phones only.
        assert!(!is_game(&work("RJ3", "SOU", true, "")));
        assert!(!is_game(&work("RJ4", "ADV", false, "")));
        let translated = Work {
            name: "英語版ゲームデータ（AI翻訳） / ENG ver. Game Data (AI-translated).".into(),
            ..work("RJ5", "RPG", true, "")
        };
        assert!(!is_game(&translated));
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
    fn reads_dlsite_dates() {
        let at = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(parse_date("1970-01-02T00:00:01.000000Z"), at(86_401));
        assert_eq!(parse_date("2017-12-09T15:00:00.000000Z"), at(1_512_831_600));
        assert_eq!(parse_date("2017-12-09T15:00:00Z"), at(1_512_831_600));
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn plans_new_games_updates_and_folders_already_current() {
        let purchases = [
            work("RJ1", "RPG", true, "2020-01-01T00:00:00.000000Z"),
            work("RJ2", "RPG", true, "2024-01-01T00:00:00.000000Z"),
            work("RJ3", "RPG", true, "2024-01-01T00:00:00.000000Z"),
            work("RJ4", "RPG", true, "2024-01-01T00:00:00.000000Z"),
            work("RJ5", "RPG", true, "2024-01-01T00:00:00.000000Z"),
            work("RJ6", "SOU", true, "2024-01-01T00:00:00.000000Z"),
        ];
        let at = |date: &str| parse_date(date).unwrap();
        let disk = |folder: &str, version: Option<&str>, modified: &str| OnDisk {
            folder: PathBuf::from(folder),
            version: version.map(str::to_owned),
            modified: at(modified),
        };
        let on_disk = HashMap::from([
            // Filled by windows-link with an older version, and with the latest.
            (
                "RJ2".to_owned(),
                disk(
                    "d2",
                    Some("2023-01-01T00:00:00.000000Z"),
                    "2023-02-01T00:00:00Z",
                ),
            ),
            (
                "RJ3".to_owned(),
                disk(
                    "d3",
                    Some("2024-01-01T00:00:00.000000Z"),
                    "2023-02-01T00:00:00Z",
                ),
            ),
            // Put there by DLsiteNest before and after the latest version came out.
            ("RJ4".to_owned(), disk("d4", None, "2023-06-01T00:00:00Z")),
            ("RJ5".to_owned(), disk("d5", None, "2024-06-01T00:00:00Z")),
        ]);
        let plan = plan(&purchases, &on_disk);
        assert_eq!(
            plan,
            Plan {
                fresh: vec![&purchases[0]],
                updates: vec![
                    (&purchases[1], PathBuf::from("d2")),
                    (&purchases[3], PathBuf::from("d4"))
                ],
                current: vec![(&purchases[4], "2024-01-01T00:00:00.000000Z".to_owned())],
            }
        );
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
