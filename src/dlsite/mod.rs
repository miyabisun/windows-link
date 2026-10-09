//! A DLsite game library read from the folders DLsiteNest makes, `<root>\<maker>\<title>`,
//! so it keeps working without DLsiteNest. With the account in `secrets.yaml`, the
//! games bought but not there yet are downloaded into the same layout, and updated
//! games are brought up to date (the library itself is `crate::shop`'s). Which DLsite
//! work each folder is comes from what was recorded, DLsiteNest's records and the
//! purchases.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use crate::{
    library::{KeyError, LicenseKey},
    secrets::DlsiteAccount,
    shop::{Known, Shop, ShopLibrary, Title, Work, folder_key},
};

pub mod nest;
pub mod play;
pub mod works;

/// Where DLsiteNest puts games.
pub const DEFAULT_ROOT: &str = r"D:\DLsiteNest\Game";

/// DLsite, signed in with the account in `secrets.yaml` that `refresh` was given.
#[derive(Default)]
pub struct DlsiteShop {
    account: Mutex<Option<DlsiteAccount>>,
}

impl DlsiteShop {
    fn account(&self) -> Option<DlsiteAccount> {
        self.account
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Shop for DlsiteShop {
    fn name(&self) -> &'static str {
        "DLsite"
    }

    fn wants(&self, work: &Work) -> bool {
        works::is_game(work)
    }

    fn ready(&self) -> bool {
        self.account().is_some()
    }

    fn released(&self, version: &str) -> Option<SystemTime> {
        works::parse_date(version)
    }

    fn fetch(
        &self,
        work: &Work,
        staging: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<PathBuf>, String> {
        let account = self.account().ok_or("secrets.yaml has no DLsite account")?;
        let session = play::sign_in(&account)?;
        let mut files = Vec::new();
        for page in session.parts(&work.id)? {
            let file = session.resolve(&page)?;
            let path = staging.join(&file.name);
            session.fetch(&file, &path, progress)?;
            files.push(path);
        }
        Ok(files)
    }

    /// Read from DLsite each time, so the keys are kept nowhere on this PC.
    fn license_keys(&self, work: &str) -> Result<Vec<LicenseKey>, KeyError> {
        let account = self
            .account()
            .ok_or_else(|| KeyError::Unavailable("secrets.yaml has no DLsite account".into()))?;
        play::sign_in(&account)
            .and_then(|session| session.license_keys(work))
            .map_err(KeyError::Unavailable)
    }
}

/// The DLsite games under one folder (`DEFAULT_ROOT` unless configured).
pub type DlsiteLibrary = ShopLibrary<DlsiteShop>;

impl ShopLibrary<DlsiteShop> {
    /// Move what the previous version knew (`dlsite_works`, by folder ID) into
    /// `dlsite_games`, then drop it. Kept while the games folder cannot be read.
    pub fn adopt_legacy_works(&self) {
        let legacy = || -> rusqlite::Result<HashMap<String, Known>> {
            let conn = self.store.conn();
            let Ok(mut stmt) = conn.prepare("SELECT item_id, work_id, image FROM dlsite_works")
            else {
                return Ok(HashMap::new());
            };
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    Known {
                        work: row.get(1)?,
                        image: row.get(2)?,
                    },
                ))
            })?;
            rows.collect()
        };
        let adopt = || -> rusqlite::Result<()> {
            let known = legacy()?;
            if known.is_empty() {
                return self
                    .store
                    .conn()
                    .execute_batch("DROP TABLE IF EXISTS dlsite_works");
            }
            let Ok(games) = self.rescan() else {
                return Ok(());
            };
            let titles: Vec<Title> = games.into_iter().map(|(title, _)| title).collect();
            self.store.remember(&titles, &known)?;
            self.store
                .conn()
                .execute_batch("DROP TABLE IF EXISTS dlsite_works")
        };
        if let Err(err) = adopt() {
            tracing::warn!(%err, "cannot move the DLsite works the previous version knew");
        }
    }

    /// Find out again which DLsite work each game is: from what was recorded, from
    /// DLsiteNest's records, and from the account's purchases when `secrets.yaml` has
    /// one. Pictures of works known only through DLsiteNest come from DLsite's public
    /// information. Failures are logged and leave what was known.
    pub fn refresh(&self, account: Option<&DlsiteAccount>) {
        *self
            .shop()
            .account
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = account.cloned();
        self.adopt_legacy_works();
        let Ok(games) = self.rescan() else {
            return;
        };
        let titles: Vec<Title> = games.into_iter().map(|(title, _)| title).collect();
        let nest = nest::read();
        let purchases = match account.map(play::fetch_purchases) {
            Some(Ok(works)) => {
                tracing::info!(works = works.len(), "DLsite purchases read");
                self.set_purchases(&works);
                works
            }
            Some(Err(reason)) => {
                tracing::warn!(%reason, "cannot read the DLsite purchases");
                Vec::new()
            }
            None => Vec::new(),
        };
        let pictured: HashSet<&str> = purchases
            .iter()
            .filter(|w| w.image.is_some())
            .map(|w| w.id.as_str())
            .collect();
        let mut missing: Vec<String> = nest
            .values()
            .filter(|id| !pictured.contains(id.as_str()))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        let images = play::fetch_public_images(&missing).unwrap_or_else(|reason| {
            tracing::warn!(%reason, "cannot read DLsite's public product information");
            HashMap::new()
        });
        let remembered = self.store.games().unwrap_or_default();
        let known = identify(&titles, &remembered, &nest, &purchases, &images);
        let unknown: Vec<&str> = titles
            .iter()
            .filter(|t| !known.contains_key(&t.id))
            .map(|t| t.name.as_str())
            .collect();
        tracing::info!(
            games = titles.len(),
            identified = known.len(),
            ?unknown,
            "DLsite games identified"
        );
        if let Err(err) = self.store.remember(&titles, &known) {
            tracing::warn!(%err, "cannot keep which DLsite work each game is");
        }
    }
}

/// A title by its letters and digits only (folder names lose characters Windows does
/// not allow), ignoring case and full-width letters.
fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            // Full-width ASCII to ASCII.
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            _ => c,
        })
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A title without what was added for a while, such as `【30%OFF!!】` or `✅…特典✅`.
fn without_sale_text(text: &str) -> String {
    let mut plain = String::new();
    let mut closing = None;
    for c in text.chars() {
        match (closing, c) {
            (None, '【') => closing = Some('】'),
            (None, '✅') => closing = Some('✅'),
            (Some(end), _) if c == end => closing = None,
            (Some(_), _) => {}
            (None, _) => plain.push(c),
        }
    }
    plain
}

/// Works given to games by title ID, each work to one game at most.
#[derive(Default)]
struct Claims {
    works: HashMap<String, String>,
    taken: HashSet<String>,
}

impl Claims {
    /// Give `work` to `title` unless either already has its match.
    fn give(&mut self, title: &Title, work: &str) {
        if !self.works.contains_key(&title.id) && self.taken.insert(work.to_owned()) {
            self.works.insert(title.id.clone(), work.to_owned());
        }
    }
}

/// Which work each game is, by title ID, each work for one game at most. In order:
/// what was `remembered` of its folder, DLsiteNest's record of it, the purchase left
/// with the same title (the only one, or the one by the same maker) as it is and then
/// without sale text, and the maker's only purchase left when the game is the maker's
/// only one left. Pictures come from the
/// purchases, else from `images` (DLsite's public information, by work ID).
#[allow(clippy::implicit_hasher, reason = "the maps are built here")]
pub fn identify(
    titles: &[Title],
    remembered: &HashMap<String, Known>,
    nest: &HashMap<String, String>,
    purchases: &[play::Work],
    images: &HashMap<String, String>,
) -> HashMap<String, Known> {
    let by_id: HashMap<&str, &play::Work> = purchases.iter().map(|w| (w.id.as_str(), w)).collect();
    let mut claims = Claims::default();
    for title in titles {
        if let Some(known) = remembered.get(&folder_key(&title.folder)) {
            claims.give(title, &known.work);
        }
    }
    for title in titles {
        if let Some(work) = nest.get(&folder_key(&title.folder)) {
            claims.give(title, work);
        }
    }
    let keys: [fn(&str) -> String; 2] = [fold, |text| fold(&without_sale_text(text))];
    for key in keys {
        let mut by_title: HashMap<String, Vec<&play::Work>> = HashMap::new();
        for work in purchases {
            by_title.entry(key(&work.name)).or_default().push(work);
        }
        for title in titles {
            if claims.works.contains_key(&title.id) {
                continue;
            }
            let same: Vec<&play::Work> = by_title
                .get(&key(&title.name))
                .into_iter()
                .flatten()
                .copied()
                .filter(|w| !claims.taken.contains(&w.id))
                .collect();
            let maker = fold(&title.maker);
            let work = match same.as_slice() {
                [only] => Some(*only),
                several => several.iter().copied().find(|w| fold(&w.maker) == maker),
            };
            if let Some(work) = work {
                claims.give(title, &work.id);
            }
        }
    }
    let mut left: HashMap<String, (Vec<&Title>, Vec<&play::Work>)> = HashMap::new();
    for title in titles.iter().filter(|t| !claims.works.contains_key(&t.id)) {
        left.entry(fold(&title.maker)).or_default().0.push(title);
    }
    for work in purchases {
        if !claims.taken.contains(&work.id)
            && let Some((_, theirs)) = left.get_mut(&fold(&work.maker))
        {
            theirs.push(work);
        }
    }
    for (games, theirs) in left.values() {
        if let ([game], [work]) = (games.as_slice(), theirs.as_slice()) {
            claims.give(game, &work.id);
        }
    }
    claims
        .works
        .into_iter()
        .map(|(title, work)| {
            let image = by_id
                .get(work.as_str())
                .and_then(|w| w.image.clone())
                .or_else(|| images.get(&work).cloned());
            (title, Known { work, image })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use super::{DlsiteLibrary, DlsiteShop};
    use crate::library::{GameLibrary, Label, LabelError, StartError};
    use crate::shop::{ShopStore, Title, candidates, default_program, listing, title_id};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn games_are_identified_by_record_then_dlsitenest_then_by_name_then_by_maker() {
        use super::{identify, play::Work};
        use crate::shop::Known;

        let titles = [
            title("SPLUSH WAVE", "Grand Order 戦記", 1),
            title("サークル", "雨瀬みゆを全国大会につれてって！", 1),
            title("Other", "ＦＵＬＬ　ｗｉｄｔｈ: Title", 1),
            title("Unknown", "Nobody bought this", 1),
            title("Twice", "Same Name", 1),
            title(
                "ちょいや",
                "あらがえ!!人妻サバイバー【リリース記念30%OFF!!】",
                1,
            ),
            title(
                "にゃんにゃんソフト",
                "✅8_6まで早期限定特典✅さいみん！妹いたずらクリッカー",
                1,
            ),
            title("スーパーバッド", "夜歩き", 1),
            title("Kept", "Recorded", 1),
            title("DLC", "LOOKhac【Hシーン全解放DLC】", 1),
            title("DLC", "LOOKhac", 1),
        ];
        let path = |t: &Title| t.folder.to_string_lossy().to_lowercase();
        let remembered = HashMap::from([(
            path(&titles[8]),
            Known {
                work: "RJ8".into(),
                image: None,
            },
        )]);
        let nest = HashMap::from([
            (path(&titles[0]), "RJ01686872".to_owned()),
            // The record of this folder wins over DLsiteNest's.
            (path(&titles[8]), "RJ999".to_owned()),
        ]);
        let work = |id: &str, name: &str, maker: &str, image: Option<&str>| Work {
            id: id.into(),
            name: name.into(),
            maker: maker.into(),
            image: image.map(str::to_owned),
            ..Work::default()
        };
        let purchases = [
            work(
                "RJ2",
                "雨瀬みゆを全国大会につれてって!",
                "サークル",
                Some("https://img/2.jpg"),
            ),
            work("RJ3", "Full Width / Title", "someone else", None),
            work("RJ4", "Same Name", "A", None),
            work("RJ5", "Same Name", "B", None),
            work("RJ6", "あらがえ!!人妻サバイバー", "ちょいや", None),
            work(
                "RJ7",
                "さいみん!妹いたずらクリッカー",
                "にゃんにゃんソフト",
                None,
            ),
            work("RJ10", "夜歩き 完全版", "スーパーバッド", None),
            work("RJ11", "LOOK.hac", "DLC", None),
            work("RJ12", "LOOK.hac【Hシーン全解放DLC】", "DLC", None),
        ];
        let images = HashMap::from([("RJ01686872".to_owned(), "https://img/1.jpg".to_owned())]);
        let known = identify(&titles, &remembered, &nest, &purchases, &images);
        let got = |t: &Title| known.get(&t.id).cloned();
        let work_of = |t: &Title| got(t).map(|k| k.work);
        assert_eq!(
            got(&titles[0]),
            Some(Known {
                work: "RJ01686872".into(),
                image: Some("https://img/1.jpg".into())
            })
        );
        assert_eq!(
            got(&titles[1]),
            Some(Known {
                work: "RJ2".into(),
                image: Some("https://img/2.jpg".into())
            })
        );
        // The maker differs, but no other purchase has that title.
        assert_eq!(work_of(&titles[2]), Some("RJ3".to_owned()));
        assert_eq!(work_of(&titles[3]), None);
        // Two purchases share the title and neither maker matches: unknown.
        assert_eq!(work_of(&titles[4]), None);
        // Sale text in 【】 or between ✅ is not part of the title.
        assert_eq!(work_of(&titles[5]), Some("RJ6".to_owned()));
        assert_eq!(work_of(&titles[6]), Some("RJ7".to_owned()));
        // The maker's only unmatched purchase, for the maker's only unmatched game.
        assert_eq!(work_of(&titles[7]), Some("RJ10".to_owned()));
        assert_eq!(work_of(&titles[8]), Some("RJ8".to_owned()));
        // 【】 that is part of the title on DLsite too is kept.
        assert_eq!(work_of(&titles[9]), Some("RJ12".to_owned()));
        assert_eq!(work_of(&titles[10]), Some("RJ11".to_owned()));
    }

    #[test]
    fn ids_are_short_stable_and_differ_by_maker() {
        let id = title_id("3Djp_Art", "湿度の高い夏のマゾ");
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(id, title_id("3Djp_Art", "湿度の高い夏のマゾ"));
        assert_ne!(id, title_id("other", "湿度の高い夏のマゾ"));
    }

    #[test]
    fn candidates_leave_out_helpers_and_look_one_folder_down_when_needed() {
        assert_eq!(
            candidates(
                &names(&["NatsunoMazo.exe", "UnityCrashHandler64.exe", "unins000.exe"]),
                &[]
            ),
            ["NatsunoMazo.exe"]
        );
        assert_eq!(
            candidates(
                &[],
                &[
                    ("Windows64bit".into(), names(&["SaiminGame.exe"])),
                    ("Mac".into(), vec![])
                ]
            ),
            ["Windows64bit/SaiminGame.exe"]
        );
        // The folders below are only looked at when the game's folder has none.
        assert_eq!(
            candidates(
                &names(&["Game.exe"]),
                &[("tools".into(), names(&["Editor.exe"]))]
            ),
            ["Game.exe"]
        );
        assert!(
            candidates(
                &names(&["notification_helper.exe", "vc_redist.x64.exe"]),
                &[]
            )
            .is_empty()
        );
        assert!(
            candidates(&names(&["ファイル破損チェックツール.exe", "Game.exe"]), &[]).len() == 1
        );
    }

    #[test]
    fn the_default_is_the_only_program_or_the_only_one_that_is_not_a_tool() {
        assert_eq!(
            default_program(&names(&["Game.exe"])).map(String::as_str),
            Some("Game.exe")
        );
        assert_eq!(
            default_program(&names(&["Config.exe", "GamePro.exe"])).map(String::as_str),
            Some("GamePro.exe")
        );
        assert_eq!(
            default_program(&names(&[
                "Ntraholic.exe",
                "SetupReiPatcherAndAutoTranslator.exe"
            ]))
            .map(String::as_str),
            Some("Ntraholic.exe")
        );
        assert_eq!(
            default_program(&names(&["FallenFlower.exe", "UcModLauncher.exe"])).map(String::as_str),
            Some("FallenFlower.exe")
        );
        assert_eq!(default_program(&names(&["app.exe", "startup.exe"])), None);
        assert_eq!(
            default_program(&names(&["Game.exe", "Game_C.exe", "Game_E.exe"])),
            None
        );
        assert_eq!(default_program(&[]), None);
    }

    fn title(maker: &str, name: &str, modified: u64) -> Title {
        Title {
            id: title_id(maker, name),
            maker: maker.into(),
            name: name.into(),
            folder: PathBuf::from(format!(r"D:\Game\{maker}\{name}")),
            modified: UNIX_EPOCH + Duration::from_secs(modified),
        }
    }

    #[test]
    fn the_listing_puts_recently_started_then_recently_installed_games_first() {
        let a = title("A", "Old", 100);
        let b = title("B", "New", 300);
        let c = title("C", "Played", 200);
        let titles = vec![
            (a.clone(), names(&["Old.exe"])),
            (b.clone(), names(&["Config.exe", "Game.exe"])),
            (c.clone(), vec![]),
        ];
        let hidden = Label {
            id: "hidden".into(),
            name: "非表示".into(),
            editable: false,
        };
        let labels = vec![(hidden.clone(), HashSet::from([a.id.clone()]))];
        let started = HashMap::from([(c.id.clone(), 50)]);
        let images = HashMap::from([(b.id.clone(), "https://img/b.jpg".to_owned())]);
        let listing = listing(&titles, &labels, &started, &images);
        let rows: Vec<_> = listing
            .items
            .iter()
            .map(|i| {
                (
                    i.name.as_str(),
                    i.detail.as_deref(),
                    i.choosable,
                    i.labels.clone(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Played", Some("C"), false, vec![]),
                ("New", Some("B"), true, vec![]),
                ("Old", Some("A"), false, vec!["hidden".to_owned()]),
            ]
        );
        assert!(listing.items.iter().all(|i| i.installed));
        // A game known on DLsite shows its picture; the others their program's icon.
        assert_eq!(listing.items[1].image.as_deref(), Some("https://img/b.jpg"));
        assert_eq!(listing.items[0].image, None);
        assert_eq!(listing.labels, [hidden]);
        assert_eq!(listing.partial, None);
    }

    /// A games folder with "Maker/One" (one program), "Maker/Two" (two to choose from)
    /// and "Other/None" (no program), in a fresh temporary folder.
    fn games(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("windows-link-dlsite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (folder, files) in [
            ("Maker/One", &["One.exe", "UnityCrashHandler64.exe"][..]),
            ("Maker/Two", &["app.exe", "startup.exe"][..]),
            ("Other/None", &["readme.txt"][..]),
            // A backup DLsiteNest leaves next to a game is not a game.
            ("Maker/One.bak", &["One.exe"][..]),
            // Nor are windows-link's downloads in progress.
            (".windows-link/RJ1", &["RJ1.zip"][..]),
        ] {
            let dir = root.join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            for file in files {
                std::fs::write(dir.join(file), "").unwrap();
            }
        }
        root
    }

    fn library(root: &std::path::Path) -> DlsiteLibrary {
        DlsiteLibrary::new(
            root.to_path_buf(),
            std::sync::Arc::new(ShopStore::in_memory("dlsite").unwrap()),
            DlsiteShop::default(),
        )
    }

    #[test]
    fn updating_wakes_the_round_of_downloads_once_it_can_download() {
        use futures_util::FutureExt;

        use crate::library::{Update, UpdateError};

        let root = games("update");
        let wake = std::sync::Arc::new(tokio::sync::Notify::new());
        let library = library(&root).with_wake(wake.clone());
        // Without the account nothing can be downloaded.
        assert!(matches!(
            library.update(&[]),
            Err(UpdateError::Unavailable(_))
        ));
        assert!(wake.notified().now_or_never().is_none());

        *library.shop().account.lock().unwrap() = Some(crate::secrets::DlsiteAccount {
            login_id: "id".into(),
            password: "password".into(),
        });
        assert_eq!(library.update(&[]), Ok(Update::Round));
        assert!(wake.notified().now_or_never().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_games_folder_lists_its_games_and_starts_them() {
        let root = games("start");
        let library = library(&root);
        let listing = library.listing();
        assert_eq!(listing.items.len(), 3);
        assert_eq!(
            listing
                .labels
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            ["お気に入り", "非表示"]
        );
        let id = |name: &str| {
            listing
                .items
                .iter()
                .find(|i| i.name == name)
                .unwrap()
                .id
                .clone()
        };

        let one = library.start(&id("One")).unwrap();
        assert_eq!(PathBuf::from(&one.open), root.join(r"Maker\One\One.exe"));
        assert_eq!(one.folder, Some(root.join(r"Maker\One")));
        assert_eq!(library.start(&id("Two")), Err(StartError::Choose));
        assert_eq!(library.start(&id("None")), Err(StartError::NoProgram));
        assert_eq!(library.start("nope"), Err(StartError::NotFound));

        // A choice is remembered; only a candidate can be chosen.
        assert!(matches!(
            library.choose_program(&id("Two"), "evil.exe"),
            Err(LabelError::Invalid(_))
        ));
        library.choose_program(&id("Two"), "startup.exe").unwrap();
        let two = library.start(&id("Two")).unwrap();
        assert_eq!(
            PathBuf::from(&two.open),
            root.join(r"Maker\Two\startup.exe")
        );
        let programs = library.programs(&id("Two")).unwrap();
        assert_eq!(programs.candidates, ["app.exe", "startup.exe"]);
        assert_eq!(programs.chosen.as_deref(), Some("startup.exe"));

        // The last started game comes first.
        assert_eq!(library.listing().items[0].name, "Two");
        assert_eq!(library.folder(&id("None")), Some(root.join(r"Other\None")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_known_game_goes_by_its_work_id_and_keeps_its_labels_when_its_folder_moves() {
        use super::{identify, play::Work};
        use crate::shop::Known;

        let root = games("known");
        let library = library(&root);
        let item = |name: &str| {
            library
                .listing()
                .items
                .into_iter()
                .find(|i| i.name == name)
                .unwrap()
        };
        let hashed = item("Two").id;
        // Labels, the chosen program and the start time were kept under the folder's ID.
        library.set_label("favorite", &hashed, true).unwrap();
        library.choose_program(&hashed, "startup.exe").unwrap();
        library.start(&hashed).unwrap();
        // Pins live in the same database.
        library
            .store
            .conn()
            .execute_batch(&format!(
                "CREATE TABLE library_pins (button_id TEXT NOT NULL, item_id TEXT NOT NULL,
                     name TEXT NOT NULL, position INTEGER NOT NULL, PRIMARY KEY (button_id, item_id));
                 INSERT INTO library_pins VALUES ('dlsite', '{hashed}', 'Two', 1);"
            ))
            .unwrap();

        let identified = |library: &super::DlsiteLibrary, name: &str| {
            let games = library.rescan().unwrap();
            let titles: Vec<Title> = games.into_iter().map(|(t, _)| t).collect();
            let remembered = library.store.games().unwrap();
            let purchases = [Work {
                id: "RJ9".into(),
                name: name.into(),
                maker: "Maker".into(),
                image: Some("https://img/9.jpg".into()),
                ..Work::default()
            }];
            let known = identify(
                &titles,
                &remembered,
                &HashMap::new(),
                &purchases,
                &HashMap::new(),
            );
            library.store.remember(&titles, &known).unwrap();
        };
        identified(&library, "Two");
        let two = item("Two");
        assert_eq!(two.id, "RJ9");
        assert_eq!(two.labels, ["favorite"]);
        assert_eq!(two.image.as_deref(), Some("https://img/9.jpg"));
        assert_eq!(
            library.programs("RJ9").unwrap().chosen.as_deref(),
            Some("startup.exe")
        );
        assert_eq!(
            library.picture("RJ9"),
            Some(crate::library::Picture::Url("https://img/9.jpg".into()))
        );
        assert_eq!(library.listing().items[0].id, "RJ9");
        let pinned: String = library
            .store
            .conn()
            .query_row("SELECT item_id FROM library_pins", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pinned, "RJ9");
        // One record per work: the work ID and its folder.
        assert_eq!(
            library.store.games().unwrap(),
            HashMap::from([(
                root.join(r"Maker\Two").to_string_lossy().to_lowercase(),
                Known {
                    work: "RJ9".into(),
                    image: Some("https://img/9.jpg".into())
                }
            )])
        );

        // Renamed on disk (sale text taken out, say): the work and its labels stay.
        std::fs::rename(root.join(r"Maker\Two"), root.join(r"Maker\Two Renamed")).unwrap();
        identified(&library, "Two Renamed");
        let renamed = item("Two Renamed");
        assert_eq!(renamed.id, "RJ9");
        assert_eq!(renamed.labels, ["favorite"]);
        let works: Vec<_> = library
            .store
            .games()
            .unwrap()
            .into_iter()
            .map(|(path, k)| (path, k.work))
            .collect();
        assert_eq!(
            works,
            [(
                root.join(r"Maker\Two Renamed")
                    .to_string_lossy()
                    .to_lowercase(),
                "RJ9".to_owned()
            )]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn games_found_by_the_previous_version_move_to_the_games_table() {
        let root = games("legacy");
        let library = library(&root);
        let item = |name: &str| {
            library
                .listing()
                .items
                .into_iter()
                .find(|i| i.name == name)
                .unwrap()
        };
        let (one, two) = (item("One").id, item("Two").id);
        library.set_label("hidden", &one, true).unwrap();
        // The previous version could give one work to two folders (a copy left under
        // another maker, say): the first keeps it.
        library
            .store
            .conn()
            .execute_batch(&format!(
                "CREATE TABLE dlsite_works (item_id TEXT PRIMARY KEY, work_id TEXT NOT NULL, image TEXT);
                 INSERT INTO dlsite_works VALUES ('{one}', 'RJ5', 'https://img/5.jpg');
                 INSERT INTO dlsite_works VALUES ('{two}', 'RJ5', NULL);"
            ))
            .unwrap();
        library.adopt_legacy_works();
        let adopted = item("One");
        assert_eq!(adopted.id, "RJ5");
        assert_eq!(adopted.labels, ["hidden"]);
        assert_eq!(adopted.image.as_deref(), Some("https://img/5.jpg"));
        assert_eq!(item("Two").id, two);
        let left: i64 = library
            .store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'dlsite_works'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_games_read_from_the_folders_are_kept_in_their_order() {
        let store = ShopStore::in_memory("dlsite").unwrap();
        let title = |maker: &str, name: &str, secs| Title {
            id: title_id(maker, name),
            maker: maker.into(),
            name: name.into(),
            folder: PathBuf::from(format!(r"D:\Game\{maker}\{name}")),
            modified: UNIX_EPOCH + Duration::new(secs, 7),
        };
        let first = vec![
            (title("B", "Two", 2), names(&["a.exe", "sub/b.exe"])),
            (title("A", "One", 1), Vec::new()),
        ];
        let (game, other) = (
            std::path::Path::new(r"D:\Game"),
            std::path::Path::new(r"E:\Game"),
        );
        store.keep_scan(game, &first).unwrap();
        assert_eq!(store.scanned(game).unwrap(), first);
        let second = vec![(title("C", "Three", 3), names(&["c.exe"]))];
        store.keep_scan(game, &second).unwrap();
        assert_eq!(store.scanned(game).unwrap(), second);
        // Libraries on other folders share the database but not their games.
        store.keep_scan(other, &first).unwrap();
        assert_eq!(store.scanned(game).unwrap(), second);
        assert_eq!(store.scanned(other).unwrap(), first);
    }

    #[test]
    fn the_listing_comes_from_the_games_kept_until_the_folders_are_read_again() {
        let root = games("kept");
        let library = library(&root);
        assert_eq!(library.listing().items.len(), 3);
        let three = root.join(r"Maker\Three");
        std::fs::create_dir_all(&three).unwrap();
        std::fs::write(three.join("Three.exe"), "").unwrap();
        assert_eq!(library.listing().items.len(), 3);
        library.reread();
        let listing = library.listing();
        assert_eq!(listing.items.len(), 4);
        let id = |name: &str| {
            listing
                .items
                .iter()
                .find(|i| i.name == name)
                .unwrap()
                .id
                .clone()
        };

        // Starting reads the game's own folder again.
        std::fs::rename(three.join("Three.exe"), three.join("Three2.exe")).unwrap();
        assert_eq!(
            PathBuf::from(library.start(&id("Three")).unwrap().open),
            three.join("Three2.exe")
        );
        std::fs::remove_dir_all(root.join(r"Maker\One")).unwrap();
        assert_eq!(library.start(&id("One")), Err(StartError::NotFound));

        std::fs::remove_dir_all(&root).unwrap();
        library.reread();
        assert!(
            library
                .listing()
                .partial
                .unwrap()
                .contains("does not exist")
        );
    }

    #[test]
    fn a_download_is_remembered_with_its_version() {
        let store = ShopStore::in_memory("dlsite").unwrap();
        store
            .record_download(
                "RJ1",
                std::path::Path::new(r"D:\Game\M\T"),
                Some("https://img/1.jpg"),
                "2024-01-01",
            )
            .unwrap();
        store
            .record_download(
                "RJ1",
                std::path::Path::new(r"D:\Game\M\T"),
                None,
                "2025-01-01",
            )
            .unwrap();
        assert_eq!(
            store.versions().unwrap(),
            HashMap::from([("RJ1".to_owned(), "2025-01-01".to_owned())])
        );
        let games = store.games().unwrap();
        let known = &games[r"d:\game\m\t"];
        assert_eq!(known.work, "RJ1");
        assert_eq!(known.image.as_deref(), Some("https://img/1.jpg"));
        store.set_version("RJ1", "2026-01-01").unwrap();
        assert_eq!(store.versions().unwrap()["RJ1"], "2026-01-01");
    }

    #[test]
    fn bought_games_not_here_yet_are_listed_first_and_cannot_start() {
        use super::play::Work;

        let root = games("waiting");
        let library = library(&root);
        let bought = |id: &str, name: &str, kind: &str, windows: bool| Work {
            id: id.into(),
            name: name.into(),
            maker: "Brand".into(),
            image: Some(format!("https://img/{id}.jpg")),
            kind: kind.into(),
            windows,
            version: "2024-01-01T00:00:00.000000Z".into(),
        };
        library.set_purchases(&[
            bought("RJ7", "Coming", "RPG", true),
            bought("RJ8", "Downloading", "SLN", true),
            bought("RJ9", "Voice", "SOU", true),
            bought("RJ10", "Phone", "ADV", false),
        ]);
        library
            .progress
            .lock()
            .unwrap()
            .insert("RJ8".into(), "ダウンロード中 40%".into());
        library.set_label("favorite", "RJ7", true).unwrap();
        let listing = library.listing();
        let rows: Vec<_> = listing
            .items
            .iter()
            .map(|i| (i.id.as_str(), i.installed, i.status.as_deref()))
            .take(3)
            .collect();
        assert_eq!(
            rows,
            [
                ("RJ7", false, Some("ダウンロード待ち")),
                ("RJ8", false, Some("ダウンロード中 40%")),
                (listing.items[2].id.as_str(), true, None),
            ]
        );
        assert_eq!(listing.items.len(), 5);
        let coming = &listing.items[0];
        assert_eq!(coming.name, "Coming");
        assert_eq!(coming.detail.as_deref(), Some("Brand"));
        assert_eq!(coming.image.as_deref(), Some("https://img/RJ7.jpg"));
        assert_eq!(coming.labels, ["favorite"]);
        assert_eq!(
            library.start("RJ8"),
            Err(StartError::NotDownloaded("ダウンロード中 40%".into()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn labels_are_kept_and_the_fixed_ones_stay() {
        let root = games("labels");
        let library = library(&root);
        let one = library
            .listing()
            .items
            .iter()
            .find(|i| i.name == "One")
            .unwrap()
            .id
            .clone();
        let rpg = library.create_label("RPG").unwrap();
        assert!(rpg.editable);
        library.set_label(&rpg.id, &one, true).unwrap();
        library.set_label("hidden", &one, true).unwrap();
        library.rename_label(&rpg.id, "JRPG").unwrap();
        let listing = library.listing();
        let item = listing.items.iter().find(|i| i.id == one).unwrap();
        assert_eq!(item.labels, ["hidden".to_owned(), rpg.id.clone()]);
        assert_eq!(listing.labels[2].name, "JRPG");

        library.set_label(&rpg.id, &one, false).unwrap();
        library.delete_label(&rpg.id).unwrap();
        assert_eq!(library.delete_label(&rpg.id), Err(LabelError::NotFound));
        assert!(matches!(
            library.delete_label("hidden"),
            Err(LabelError::Invalid(_))
        ));
        assert_eq!(
            library.set_label("nope", &one, true),
            Err(LabelError::NotFound)
        );
        assert_eq!(library.listing().labels.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_or_empty_folder_says_so() {
        let missing = std::env::temp_dir().join("windows-link-dlsite-missing-folder");
        let listing = library(&missing).listing();
        assert!(listing.items.is_empty());
        assert!(listing.partial.unwrap().contains("does not exist"));

        let empty = games("empty");
        std::fs::remove_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            library(&empty)
                .listing()
                .partial
                .unwrap()
                .contains("no games")
        );
        let _ = std::fs::remove_dir_all(empty);
    }
}
