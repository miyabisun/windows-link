//! Which DLsite purchases are games for this PC and which go to which shelf, and the
//! dates DLsite writes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::shop::Work;

/// DLsite's work types of games (action, quiz, adventure, RPG, table, digital novel,
/// simulation, typing, shooting, puzzle, other games).
const GAME_KINDS: [&str; 11] = [
    "ACN", "QIZ", "ADV", "RPG", "TBL", "DNV", "SLN", "TYP", "STG", "PZL", "ETC",
];

/// Whether a purchase is a game for this PC: a game that runs on Windows, and not the
/// AI-translated data of a game in another language that comes with some purchases.
pub fn is_game(work: &Work) -> bool {
    is_game_kind(work) && work.windows && !work.name.contains("ゲームデータ（AI翻訳）")
}

/// Whether a purchase is a video to keep on this PC: a movie whose files DLsite gives to
/// download. Other works on the shelves open in DLsite Play.
pub fn is_video(work: &Work) -> bool {
    work.kind == "MOV" && work.downloadable
}

/// Whether a purchase is a game, for this PC or not.
fn is_game_kind(work: &Work) -> bool {
    GAME_KINDS.contains(&work.kind.as_str())
}

/// Which of the works bought a `dlsite.library` button holds: the games, which are
/// downloaded to this PC, or a shelf of the works that are not games, which open in
/// DLsite Play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Works {
    #[default]
    Games,
    /// The works for all ages.
    AllAges,
    /// The works with an age limit: R-18, and R-15 with them.
    R18,
}

impl Works {
    /// Whether a purchase belongs here. Only a work DLsite rates `all` is for all ages:
    /// one it rates otherwise, or not at all, never shows among those by mistake.
    pub fn holds(self, work: &Work) -> bool {
        let all_ages = work.age == "all";
        match self {
            Self::Games => is_game(work),
            Self::AllAges => !is_game_kind(work) && all_ages,
            Self::R18 => !is_game_kind(work) && !all_ages,
        }
    }

    /// The prefix of a shelf's tables in windows-link's database (`<prefix>_…`); none
    /// for the games, whose library is `super::DlsiteLibrary`.
    pub fn shelf(self) -> Option<&'static str> {
        match self {
            Self::Games => None,
            Self::AllAges => Some("dlsite_all"),
            Self::R18 => Some("dlsite_r18"),
        }
    }
}

/// A time DLsite gives, such as `2017-12-09T15:00:00.000000Z` (UTC) or
/// `2026-01-02T03:04:05+09:00` (a purchase, in Japan's time).
pub fn parse_date(text: &str) -> Option<SystemTime> {
    let number = |at: std::ops::Range<usize>| text.get(at)?.parse::<i64>().ok();
    if text.get(4..5)? != "-" || text.get(10..11)? != "T" {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    // Past the seconds and their fraction: `Z`, `±HH:MM`, or nothing (UTC).
    let zone = text
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone.as_bytes().first() {
        None | Some(b'Z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let hours = zone.get(1..3)?.parse::<i64>().ok()?;
            let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
            let east = (hours * 60 + minutes) * 60;
            if sign == b'+' { east } else { -east }
        }
        Some(_) => return None,
    };
    let seconds = number(11..13)? * 3600 + number(14..16)? * 60 + number(17..19)? - offset;
    // Days since 1970-01-01, by Howard Hinnant's days_from_civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    let since = u64::try_from(days * 86_400 + seconds).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(since))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use super::{Works, is_game, is_video, parse_date};
    use crate::shop::{
        Work,
        download::{OnDisk, Plan, plan},
    };

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
    fn works_that_are_not_games_go_to_a_shelf_by_their_age() {
        let voice = |age: &str| Work {
            age: age.into(),
            ..work("RJ1", "SOU", false, "")
        };
        assert!(Works::Games.holds(&work("RJ2", "RPG", true, "")));
        assert!(!Works::Games.holds(&voice("all")));
        assert!(Works::AllAges.holds(&voice("all")));
        assert!(!Works::R18.holds(&voice("all")));
        // R-15, and a rating DLsite does not give or names newly, go with R18: a work
        // never shows among the all-ages ones by mistake.
        for age in ["r18", "r15", "", "r20"] {
            assert!(Works::R18.holds(&voice(age)), "{age:?}");
            assert!(!Works::AllAges.holds(&voice(age)), "{age:?}");
        }
        // A game is on no shelf, also one that is not for this PC.
        let phone_game = Work {
            age: "all".into(),
            ..work("RJ3", "RPG", false, "")
        };
        assert!(!Works::Games.holds(&phone_game));
        assert!(!Works::AllAges.holds(&phone_game));
        assert!(!Works::R18.holds(&phone_game));
        // Each shelf has tables of its own, apart from the games' (`dlsite_…`).
        assert_eq!(Works::Games.shelf(), None);
        let (all, r18) = (Works::AllAges.shelf().unwrap(), Works::R18.shelf().unwrap());
        assert!(all != r18 && all != "dlsite" && r18 != "dlsite");
    }

    #[test]
    fn a_video_is_a_movie_whose_files_can_be_downloaded() {
        let movie = |downloadable: bool| Work {
            downloadable,
            ..work("RJ1", "MOV", false, "")
        };
        assert!(is_video(&movie(true)));
        // One with no file to download opens in DLsite Play like the other works.
        assert!(!is_video(&movie(false)));
        // So does a voice work, though its files can be downloaded.
        let voice = Work {
            downloadable: true,
            ..work("RJ2", "SOU", true, "")
        };
        assert!(!is_video(&voice));
        // A video is on a shelf, never among the games.
        assert!(!is_game(&movie(true)));
    }

    #[test]
    fn reads_dlsite_dates() {
        let at = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(parse_date("1970-01-02T00:00:01.000000Z"), at(86_401));
        assert_eq!(parse_date("2017-12-09T15:00:00.000000Z"), at(1_512_831_600));
        assert_eq!(parse_date("2017-12-09T15:00:00Z"), at(1_512_831_600));
        // Purchases are written in Japan's time.
        assert_eq!(
            parse_date("2026-01-02T03:04:05+09:00"),
            parse_date("2026-01-01T18:04:05Z")
        );
        assert_eq!(
            parse_date("2026-01-01T18:04:05.5-01:30"),
            parse_date("2026-01-01T19:34:05Z")
        );
        assert_eq!(parse_date("2026-01-01T18:04:05+9"), None);
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
        // DLsite plans the games among the purchases, by its own dates.
        let games: Vec<Work> = purchases.iter().filter(|w| is_game(w)).cloned().collect();
        let plan = plan(&games, &on_disk, &parse_date);
        assert_eq!(
            plan,
            Plan {
                fresh: vec![&games[0]],
                updates: vec![
                    (&games[1], PathBuf::from("d2")),
                    (&games[3], PathBuf::from("d4"))
                ],
                current: vec![(&games[4], "2024-01-01T00:00:00.000000Z".to_owned())],
            }
        );
    }
}
