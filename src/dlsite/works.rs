//! Which DLsite purchases are games for this PC, and the dates DLsite writes.

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
    GAME_KINDS.contains(&work.kind.as_str())
        && work.windows
        && !work.name.contains("ゲームデータ（AI翻訳）")
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

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use super::{is_game, parse_date};
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
