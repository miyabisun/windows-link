//! The works bought on DLsite that are not games, as a library: a shelf of the all-ages
//! ones, or of the rest. A work opens in DLsite Play, in the default browser, and nothing
//! of it is on this PC; but a video (`works::is_video`) is downloaded by the videos'
//! library the shelf goes with, and opens as its folder. The works are kept in
//! windows-link's database as last read, so the list shows before the purchases are read
//! and while signed out. Labels and when each work was opened are the shelf's own.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};

use rusqlite::params;
use tokio::sync::Notify;

use super::{
    DlsiteLibrary,
    session::DlsiteSession,
    works::{Works, is_video, parse_date},
};
use crate::{
    library::{
        Doing, GameLibrary, Item, Label, LabelError, Listing, Picture, Round, Start, StartError,
        Update, UpdateError,
    },
    shop::{ShopStore, WAITING, Work, unix_seconds},
};

/// The status of a work DLsite Play cannot show.
const UNPLAYABLE: &str = "ブラウザ再生に非対応";

/// Whether a work ID or a floor's name is letters and digits only, as DLsite writes
/// them: anything else stays out of an address.
fn plain(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A work's page in the shop, on the floor that sells it.
pub fn page(work: &Work) -> Option<String> {
    (plain(&work.site) && plain(&work.id)).then(|| {
        format!(
            "https://www.dlsite.com/{}/work/=/product_id/{}.html",
            work.site, work.id
        )
    })
}

/// What opening a work opens: DLsite Play, or the work's page in the shop when DLsite
/// Play cannot show it. Nothing for an ID that is not a plain work ID.
pub fn opens(work: &Work) -> Option<String> {
    let play = || plain(&work.id).then(|| format!("https://play.dlsite.com/work/{}", work.id));
    if work.playable {
        play()
    } else {
        page(work).or_else(play)
    }
}

/// The listing: the works opened last first, then those bought last. `videos` are the
/// videos' library's items by work ID: a video is here as its item says, and waits to be
/// downloaded while the library does not know it.
#[allow(clippy::implicit_hasher, reason = "the maps come from the stores")]
pub fn listing(
    works: &[Work],
    labels: &[(Label, HashSet<String>)],
    started: &HashMap<String, i64>,
    videos: &HashMap<String, Item>,
) -> Listing {
    // Whether a work is here, and how getting or updating it goes.
    let on_this_pc = |work: &Work| match videos.get(&work.id) {
        _ if !is_video(work) => (true, (!work.playable).then(|| UNPLAYABLE.to_owned())),
        Some(video) => (video.installed, video.status.clone()),
        None => (false, Some(WAITING.to_owned())),
    };
    let mut order: Vec<&Work> = works.iter().collect();
    order.sort_by(|a, b| {
        let last = |work: &Work| started.get(&work.id).copied().unwrap_or(i64::MIN);
        last(b)
            .cmp(&last(a))
            .then_with(|| b.bought.cmp(&a.bought))
            .then_with(|| a.name.cmp(&b.name))
    });
    Listing {
        items: order
            .into_iter()
            .map(|work| {
                let (installed, status) = on_this_pc(work);
                Item {
                    id: work.id.clone(),
                    name: work.name.clone(),
                    detail: Some(work.maker.clone()),
                    choosable: false,
                    image: work.image.clone(),
                    installed,
                    labels: labels
                        .iter()
                        .filter(|(_, items)| items.contains(&work.id))
                        .map(|(label, _)| label.id.clone())
                        .collect(),
                    status,
                    updated: parse_date(&work.version).and_then(unix_seconds),
                    bought: work.bought,
                    page: page(work),
                }
            })
            .collect(),
        labels: labels.iter().map(|(label, _)| label.clone()).collect(),
        ..Listing::default()
    }
}

/// One of DLsite's shelves: the works of `which` among those bought.
pub struct Shelf {
    which: Works,
    /// The shelf's own tables (`Works::shelf`): its works, labels and start times.
    store: Arc<ShopStore>,
    session: Arc<DlsiteSession>,
    /// The round under way, else the last one, as far as the shelf itself goes: the
    /// reading of the purchases. `round` adds the downloads of its videos.
    round: Mutex<Round>,
    /// Wakes DLsite's round, which reads the purchases again.
    wake: Arc<Notify>,
    /// The library that keeps the shelf's videos on this PC (and those of the other
    /// shelf, when both name the same folder).
    videos: Arc<DlsiteLibrary>,
}

impl Shelf {
    /// The shelf of `which` works with what `store` keeps of it, whose videos `videos`
    /// keeps on this PC.
    pub fn new(
        which: Works,
        store: Arc<ShopStore>,
        session: Arc<DlsiteSession>,
        wake: Arc<Notify>,
        videos: Arc<DlsiteLibrary>,
    ) -> rusqlite::Result<Self> {
        {
            let conn = store.conn();
            conn.execute_batch(&store.sql(
                "CREATE TABLE IF NOT EXISTS shop_shelf (
                     work_id  TEXT PRIMARY KEY,
                     name     TEXT NOT NULL,
                     maker    TEXT NOT NULL,
                     image    TEXT,
                     kind     TEXT NOT NULL,
                     windows  INTEGER NOT NULL,
                     version  TEXT NOT NULL,
                     bought   INTEGER,
                     age      TEXT NOT NULL,
                     site     TEXT NOT NULL,
                     playable INTEGER NOT NULL
                 );",
            ))?;
            // 0.1.57's table does not say whether a work's files can be downloaded. Its
            // works are taken as downloadable until the purchases are read again, so the
            // videos kept then are videos at once.
            let says: bool = conn.query_row(
                &store.sql(
                    "SELECT EXISTS (SELECT 1 FROM pragma_table_info('shop_shelf')
                                    WHERE name = 'downloadable')",
                ),
                [],
                |row| row.get(0),
            )?;
            if !says {
                conn.execute_batch(&store.sql(
                    "ALTER TABLE shop_shelf ADD COLUMN downloadable INTEGER NOT NULL DEFAULT 1",
                ))?;
            }
        }
        Ok(Self {
            which,
            store,
            session,
            round: Mutex::default(),
            wake,
            videos,
        })
    }

    fn round_state(&self) -> MutexGuard<'_, Round> {
        self.round.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Begin a round: the purchases are read.
    pub(crate) fn begin_round(&self) {
        *self.round_state() = Round {
            doing: Some(Doing::Purchases),
            started: unix_seconds(SystemTime::now()),
            ..Round::default()
        };
    }

    /// Take a reading of the purchases: keep the shelf's works among them, or say why
    /// they could not be read and leave the works as they were.
    pub(crate) fn refresh(&self, read: &Result<Vec<Work>, String>) {
        match read {
            Ok(purchases) => self.keep(purchases),
            Err(reason) => self.round_state().purchases = Some(reason.clone()),
        }
    }

    pub fn end_round(&self) {
        let mut round = self.round_state();
        round.doing = None;
        round.finished = unix_seconds(SystemTime::now());
    }

    /// Keep the shelf's works among `purchases` in place of those kept before.
    pub fn keep(&self, purchases: &[Work]) {
        let replace = || -> rusqlite::Result<()> {
            let mut conn = self.store.conn();
            let saving = conn.transaction()?;
            saving.execute(&self.store.sql("DELETE FROM shop_shelf"), [])?;
            for work in purchases.iter().filter(|work| self.which.holds(work)) {
                saving.execute(
                    &self.store.sql(
                        "INSERT OR REPLACE INTO shop_shelf (work_id, name, maker, image, kind,
                             windows, version, bought, age, site, playable, downloadable)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    ),
                    params![
                        work.id,
                        work.name,
                        work.maker,
                        work.image,
                        work.kind,
                        work.windows,
                        work.version,
                        work.bought,
                        work.age,
                        work.site,
                        work.playable,
                        work.downloadable
                    ],
                )?;
            }
            saving.commit()
        };
        if let Err(err) = replace() {
            tracing::warn!(shelf = ?self.which, %err, "cannot keep the DLsite works");
        }
    }

    /// The works kept, in the order they were read.
    fn works(&self) -> Vec<Work> {
        let read = || -> rusqlite::Result<Vec<Work>> {
            let conn = self.store.conn();
            let mut stmt = conn.prepare(&self.store.sql(
                "SELECT work_id, name, maker, image, kind, windows, version, bought, age, site,
                        playable, downloadable
                 FROM shop_shelf ORDER BY rowid",
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok(Work {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    maker: row.get(2)?,
                    image: row.get(3)?,
                    kind: row.get(4)?,
                    windows: row.get(5)?,
                    version: row.get(6)?,
                    bought: row.get(7)?,
                    age: row.get(8)?,
                    site: row.get(9)?,
                    playable: row.get(10)?,
                    downloadable: row.get(11)?,
                })
            })?;
            rows.collect()
        };
        read().unwrap_or_else(|err| {
            tracing::warn!(shelf = ?self.which, %err, "cannot read the DLsite works kept");
            Vec::new()
        })
    }

    fn find(&self, id: &str) -> Option<Work> {
        self.works().into_iter().find(|work| work.id == id)
    }
}

impl GameLibrary for Shelf {
    fn listing(&self) -> Listing {
        let labels = self.store.labels().unwrap_or_default();
        let started = self.store.started().unwrap_or_default();
        let videos: HashMap<String, Item> = self
            .videos
            .listing()
            .items
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect();
        Listing {
            sign_in: self.sign_in().map(str::to_owned),
            ..listing(&self.works(), &labels, &started, &videos)
        }
    }

    /// A video opens as its folder (in Explorer) once it is here; another work opens on
    /// the web.
    fn start(&self, id: &str) -> Result<Start, StartError> {
        let work = self.find(id).ok_or(StartError::NotFound)?;
        let open = if is_video(&work) {
            let folder = self
                .videos
                .folder(id)
                .ok_or_else(|| StartError::NotDownloaded(self.videos.waits(id)))?;
            folder.to_string_lossy().into_owned()
        } else {
            opens(&work).ok_or(StartError::NotFound)?
        };
        let _ = self.store.mark_started(id);
        Ok(Start { open, folder: None })
    }

    fn picture(&self, id: &str) -> Option<Picture> {
        self.find(id)?.image.map(Picture::Url)
    }

    /// A video's folder once it is here; nothing of another work is on this PC.
    fn folder(&self, id: &str) -> Option<PathBuf> {
        is_video(&self.find(id)?)
            .then(|| self.videos.folder(id))
            .flatten()
    }

    fn create_label(&self, name: &str) -> Result<Label, LabelError> {
        self.store.library_create_label(name)
    }

    fn rename_label(&self, label: &str, name: &str) -> Result<(), LabelError> {
        self.store.library_rename_label(label, name)
    }

    fn delete_label(&self, label: &str) -> Result<(), LabelError> {
        self.store.library_delete_label(label)
    }

    fn set_label(&self, label: &str, item: &str, on: bool) -> Result<(), LabelError> {
        self.store.library_set_label(label, item, on)
    }

    fn sign_in(&self) -> Option<&'static str> {
        (!self.session.signed_in()).then_some("dlsite")
    }

    /// Read the purchases again now, with DLsite's round.
    fn update(&self, _hide: &[String]) -> Result<Update, UpdateError> {
        if self.sign_in().is_some() {
            return Err(UpdateError::SignIn);
        }
        self.wake.notify_one();
        Ok(Update::Round)
    }

    /// The reading of the purchases, and the downloads of the shelf's own videos (the
    /// videos' library may be downloading the other shelf's too, which are not named).
    fn round(&self) -> Option<Round> {
        let mut round = self.round_state().clone();
        if let Some(videos) = self.videos.round() {
            let mine: HashSet<String> = self.works().into_iter().map(|work| work.id).collect();
            round.jobs = videos
                .jobs
                .into_iter()
                .filter(|job| mine.contains(&job.id))
                .collect();
            round.folder = videos.folder;
            if videos.doing == Some(Doing::Downloads) && !round.jobs.is_empty() {
                round.doing = videos.doing;
            }
        }
        Some(round)
    }

    fn page(&self, id: &str) -> Option<String> {
        page(&self.find(id)?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        sync::Arc,
    };

    use futures_util::FutureExt;

    use super::{Shelf, listing, opens, page};
    use crate::{
        dlsite::{DlsiteLibrary, DlsiteShop, VIDEO_TABLES, session::DlsiteSession, works::Works},
        library::{
            Doing, GameLibrary, Item, Job, Label, Picture, Start, StartError, Update, UpdateError,
        },
        shop::{ShopStore, Work},
    };

    /// A voice work DLsite Play shows, on the adult floor.
    fn work(id: &str, name: &str, age: &str, bought: Option<i64>) -> Work {
        Work {
            id: id.into(),
            name: name.into(),
            maker: "サークル".into(),
            image: Some(format!("https://img/{id}.jpg")),
            kind: "SOU".into(),
            version: "2025-11-09T15:00:00.000000Z".into(),
            bought,
            age: age.into(),
            site: "maniax".into(),
            playable: true,
            ..Work::default()
        }
    }

    /// A session not signed in, kept in a file of the test's own.
    fn session(tag: &str) -> Arc<DlsiteSession> {
        let file = std::env::temp_dir().join(format!(
            "windows-link-dlsite-shelf-{tag}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        Arc::new(DlsiteSession::open(file))
    }

    fn store(which: Works) -> Arc<ShopStore> {
        Arc::new(ShopStore::in_memory(which.shelf().unwrap()).unwrap())
    }

    /// A video whose files can be downloaded, on the adult floor.
    fn video(id: &str, name: &str, age: &str, bought: Option<i64>) -> Work {
        Work {
            kind: "MOV".into(),
            downloadable: true,
            ..work(id, name, age, bought)
        }
    }

    /// A videos' folder of the test's own with the video `RJ5` in `Maker\Here`, and the
    /// library of both shelves' videos on it.
    fn videos(tag: &str, session: &Arc<DlsiteSession>) -> (PathBuf, Arc<DlsiteLibrary>) {
        let root = std::env::temp_dir().join(format!(
            "windows-link-dlsite-shelf-videos-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let here = root.join("Maker").join("Here");
        std::fs::create_dir_all(&here).unwrap();
        std::fs::write(here.join("movie.mp4"), "").unwrap();
        let library = DlsiteLibrary::new(
            root.clone(),
            Arc::new(ShopStore::in_memory(VIDEO_TABLES).unwrap()),
            DlsiteShop::videos(session.clone(), vec![Works::AllAges, Works::R18]),
        );
        library
            .store
            .record_download("RJ5", &here, None, "2025-11-09T15:00:00.000000Z")
            .unwrap();
        library.rescan().unwrap();
        (root, Arc::new(library))
    }

    /// A shelf with a videos' library of its own, on an empty folder.
    fn shelf(which: Works, store: Arc<ShopStore>, session: Arc<DlsiteSession>) -> Shelf {
        let (_, videos) = videos(which.shelf().unwrap(), &session);
        Shelf::new(which, store, session, Arc::default(), videos).unwrap()
    }

    fn ids(shelf: &Shelf) -> Vec<String> {
        shelf.listing().items.into_iter().map(|i| i.id).collect()
    }

    #[test]
    fn the_listing_puts_the_works_opened_last_then_bought_last_first() {
        let unplayable = Work {
            playable: false,
            ..work("RJ5", "E", "r18", None)
        };
        let works = [
            work("RJ1", "B", "r18", Some(10)),
            work("RJ2", "A", "r18", Some(30)),
            work("RJ3", "C", "r18", Some(1)),
            work("RJ4", "D", "r18", Some(30)),
            unplayable,
        ];
        let favorite = Label {
            id: "favorite".into(),
            name: "お気に入り".into(),
            editable: false,
        };
        let labels = vec![(favorite.clone(), HashSet::from(["RJ2".to_owned()]))];
        let started = HashMap::from([("RJ3".to_owned(), 1)]);
        let listing = listing(&works, &labels, &started, &HashMap::new());
        let order: Vec<&str> = listing.items.iter().map(|i| i.id.as_str()).collect();
        // The one opened, then by purchase (the same day by name), the unknown day last.
        assert_eq!(order, ["RJ3", "RJ2", "RJ4", "RJ1", "RJ5"]);
        assert_eq!(
            listing.items[1],
            Item {
                id: "RJ2".into(),
                name: "A".into(),
                detail: Some("サークル".into()),
                choosable: false,
                image: Some("https://img/RJ2.jpg".into()),
                installed: true,
                labels: vec!["favorite".into()],
                status: None,
                updated: Some(1_762_700_400),
                bought: Some(30),
                page: Some("https://www.dlsite.com/maniax/work/=/product_id/RJ2.html".into()),
            }
        );
        // A work DLsite Play cannot show says so.
        assert_eq!(
            listing.items[4].status.as_deref(),
            Some("ブラウザ再生に非対応")
        );
        assert_eq!(listing.labels, [favorite]);
    }

    #[test]
    fn a_video_is_listed_as_here_or_with_how_getting_it_goes() {
        // What the videos' library lists: a video here, one being updated, one coming.
        let on_disk = |id: &str, installed: bool, status: Option<&str>| {
            let item = Item {
                id: id.into(),
                name: String::new(),
                detail: None,
                choosable: false,
                image: None,
                installed,
                labels: Vec::new(),
                status: status.map(str::to_owned),
                updated: None,
                bought: None,
                page: None,
            };
            (id.to_owned(), item)
        };
        let videos = HashMap::from([
            on_disk("RJ1", true, None),
            on_disk("RJ2", true, Some("更新中 40%")),
            on_disk("RJ3", false, Some("ダウンロード中 10%")),
            on_disk("RJ5", true, None),
        ]);
        // A program DLsite Play cannot show is a video like the others: its folder opens.
        let program = Work {
            playable: false,
            ..video("RJ5", "E", "r18", Some(5))
        };
        let works = [
            video("RJ1", "A", "r18", Some(9)),
            video("RJ2", "B", "r18", Some(8)),
            video("RJ3", "C", "r18", Some(7)),
            // Not in the folder and not planned yet (before the purchases are read).
            video("RJ4", "D", "r18", Some(6)),
            program,
            // A voice work is not on this PC and shows as before.
            work("RJ6", "F", "r18", Some(4)),
        ];
        let listing = listing(&works, &[], &HashMap::new(), &videos);
        let rows: Vec<(&str, bool, Option<&str>)> = listing
            .items
            .iter()
            .map(|i| (i.id.as_str(), i.installed, i.status.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("RJ1", true, None),
                ("RJ2", true, Some("更新中 40%")),
                ("RJ3", false, Some("ダウンロード中 10%")),
                ("RJ4", false, Some("ダウンロード待ち")),
                ("RJ5", true, None),
                ("RJ6", true, None),
            ]
        );
        // A video keeps what every work has.
        let first = &listing.items[0];
        assert_eq!(first.detail.as_deref(), Some("サークル"));
        assert_eq!(first.bought, Some(9));
        assert_eq!(first.updated, Some(1_762_700_400));
        assert_eq!(
            first.page.as_deref(),
            Some("https://www.dlsite.com/maniax/work/=/product_id/RJ1.html")
        );
    }

    #[test]
    fn a_video_opens_its_folder_once_it_is_here() {
        let session = session("video");
        let (root, videos) = videos("opens", &session);
        let shelf = Shelf::new(
            Works::R18,
            store(Works::R18),
            session,
            Arc::default(),
            videos.clone(),
        )
        .unwrap();
        // A reading of the purchases goes to the shelf and to its videos' library.
        let purchases = [
            video("RJ5", "Here", "r18", Some(2)),
            video("RJ6", "Coming", "r18", Some(1)),
            work("RJ1", "Voice", "r18", Some(3)),
        ];
        shelf.keep(&purchases);
        videos.set_purchases(&purchases);
        let here = root.join("Maker").join("Here");
        assert_eq!(
            shelf.start("RJ5"),
            Ok(Start {
                open: here.to_string_lossy().into_owned(),
                folder: None,
            })
        );
        assert_eq!(shelf.folder("RJ5"), Some(here.clone()));
        // The folder just opened comes first in the list.
        assert_eq!(ids(&shelf), ["RJ5", "RJ1", "RJ6"]);
        // A video not here yet says how getting it goes, and has no folder.
        assert_eq!(
            shelf.start("RJ6"),
            Err(StartError::NotDownloaded("ダウンロード待ち".into()))
        );
        videos
            .progress
            .lock()
            .unwrap()
            .insert("RJ6".into(), "ダウンロード中 40%".into());
        assert_eq!(
            shelf.start("RJ6"),
            Err(StartError::NotDownloaded("ダウンロード中 40%".into()))
        );
        let coming = shelf.listing().items.pop().unwrap();
        assert_eq!(
            (
                coming.id.as_str(),
                coming.installed,
                coming.status.as_deref()
            ),
            ("RJ6", false, Some("ダウンロード中 40%"))
        );
        assert_eq!(shelf.folder("RJ6"), None);
        // A voice work still opens in DLsite Play and has no folder.
        assert_eq!(
            shelf.start("RJ1").unwrap().open,
            "https://play.dlsite.com/work/RJ1"
        );
        assert_eq!(shelf.folder("RJ1"), None);
        // A folder that went away (deleted by hand) is not opened.
        std::fs::remove_dir_all(&here).unwrap();
        assert!(matches!(
            shelf.start("RJ5"),
            Err(StartError::NotDownloaded(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_shelfs_round_tells_the_downloads_of_its_own_videos() {
        let session = session("jobs");
        let (root, videos) = videos("jobs", &session);
        let shelf = Shelf::new(
            Works::AllAges,
            store(Works::AllAges),
            session.clone(),
            Arc::default(),
            videos.clone(),
        )
        .unwrap();
        shelf.begin_round();
        shelf.refresh(&Ok(vec![
            video("RJ7", "Mine", "all", Some(1)),
            video("RJ8", "The other shelf's", "r18", Some(2)),
        ]));
        // The library downloads both shelves' videos; this shelf names only its own.
        videos.plan_round(&[
            ("RJ8".to_owned(), "The other shelf's".to_owned()),
            ("RJ7".to_owned(), "Mine".to_owned()),
        ]);
        videos
            .progress
            .lock()
            .unwrap()
            .insert("RJ7".into(), "ダウンロード中 40%".into());
        let round = shelf.round().unwrap();
        assert_eq!(round.doing, Some(Doing::Downloads));
        assert_eq!(
            round.jobs,
            [Job {
                id: "RJ7".into(),
                name: "Mine".into(),
                status: "ダウンロード中 40%".into(),
            }]
        );
        videos.job_done("RJ7");
        videos.end_round();
        shelf.end_round();
        let round = shelf.round().unwrap();
        assert_eq!(round.doing, None);
        assert!(round.finished.is_some());
        assert_eq!(round.jobs[0].status, "完了");
        assert_eq!(round.folder, None);

        // The videos' folder gone (a disk that dropped off): nothing is downloaded, and
        // the shelf's round says why.
        let cookie = crate::shop::cookies::Cookie {
            name: "__DLsite_SID".into(),
            value: "v".into(),
            domain: "dlsite.com".into(),
            path: "/".into(),
            expires: None,
            secure: true,
            http_only: true,
        };
        session.keep(vec![cookie]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        shelf.begin_round();
        videos.begin_round();
        videos.download();
        shelf.end_round();
        let round = shelf.round().unwrap();
        assert_eq!(round.jobs, []);
        assert!(round.folder.unwrap().contains("does not exist"));
    }

    #[test]
    fn a_shelf_kept_by_the_previous_version_takes_its_videos_as_downloadable() {
        let store = store(Works::R18);
        // The table as 0.1.57 made it, which did not keep whether a work's files can be
        // downloaded.
        store
            .conn()
            .execute_batch(
                "CREATE TABLE dlsite_r18_shelf (
                     work_id TEXT PRIMARY KEY, name TEXT NOT NULL, maker TEXT NOT NULL,
                     image TEXT, kind TEXT NOT NULL, windows INTEGER NOT NULL,
                     version TEXT NOT NULL, bought INTEGER, age TEXT NOT NULL,
                     site TEXT NOT NULL, playable INTEGER NOT NULL
                 );
                 INSERT INTO dlsite_r18_shelf VALUES
                     ('RJ6', 'Video', 'M', NULL, 'MOV', 0, '', 2, 'r18', 'maniax', 1),
                     ('RJ1', 'Voice', 'M', NULL, 'SOU', 0, '', 1, 'r18', 'maniax', 1);",
            )
            .unwrap();
        let shelf = shelf(Works::R18, store, session("previous"));
        let rows: Vec<(String, bool)> = shelf
            .listing()
            .items
            .into_iter()
            .map(|i| (i.id, i.installed))
            .collect();
        // The video is one to download (not here yet); the voice work is as it was.
        assert_eq!(rows, [("RJ6".to_owned(), false), ("RJ1".to_owned(), true)]);
        // And the next reading is kept with what DLsite says.
        shelf.keep(&[Work {
            downloadable: false,
            ..video("RJ6", "Video", "r18", Some(2))
        }]);
        assert!(shelf.listing().items[0].installed);
    }

    #[test]
    fn a_work_opens_in_dlsite_play_or_on_its_page_when_play_cannot_show_it() {
        let voice = work("RJ1", "Voice", "all", None);
        assert_eq!(
            opens(&voice).as_deref(),
            Some("https://play.dlsite.com/work/RJ1")
        );
        // Each floor of the shop has the pages of its works.
        let comic = Work {
            site: "books".into(),
            ..work("BJ2", "Comic", "r18", None)
        };
        assert_eq!(
            page(&comic).as_deref(),
            Some("https://www.dlsite.com/books/work/=/product_id/BJ2.html")
        );
        let program = Work {
            playable: false,
            ..work("RJ3", "Program", "r18", None)
        };
        assert_eq!(
            opens(&program).as_deref(),
            Some("https://www.dlsite.com/maniax/work/=/product_id/RJ3.html")
        );
        // Without a floor there is no page to name, and DLsite Play is all there is.
        let lost = Work {
            site: String::new(),
            ..program
        };
        assert_eq!(page(&lost), None);
        assert_eq!(
            opens(&lost).as_deref(),
            Some("https://play.dlsite.com/work/RJ3")
        );
        // An ID that is not a plain work ID names no address at all.
        let odd = work("RJ1/../../login?next=x", "Odd", "all", None);
        assert_eq!(opens(&odd), None);
        assert_eq!(page(&odd), None);
    }

    #[test]
    fn a_shelf_keeps_the_works_of_its_age_and_opens_them() {
        let store = store(Works::R18);
        let shelf = shelf(Works::R18, store.clone(), session("keeps"));
        // Before the purchases are read there is nothing, and the list says to sign in.
        assert!(shelf.listing().items.is_empty());
        assert_eq!(shelf.listing().sign_in.as_deref(), Some("dlsite"));
        let game = Work {
            kind: "RPG".into(),
            windows: true,
            ..work("RJ3", "Game", "r18", Some(3))
        };
        shelf.keep(&[
            work("RJ1", "Voice", "r18", Some(1)),
            work("RJ2", "All ages", "all", Some(2)),
            game,
        ]);
        assert_eq!(ids(&shelf), ["RJ1"]);
        assert_eq!(
            shelf.start("RJ1"),
            Ok(Start {
                open: "https://play.dlsite.com/work/RJ1".into(),
                folder: None,
            })
        );
        // The all-ages work is on the other shelf.
        assert_eq!(shelf.start("RJ2"), Err(StartError::NotFound));
        assert_eq!(shelf.folder("RJ1"), None);
        assert_eq!(
            shelf.picture("RJ1"),
            Some(Picture::Url("https://img/RJ1.jpg".into()))
        );
        assert_eq!(
            shelf.page("RJ1").as_deref(),
            Some("https://www.dlsite.com/maniax/work/=/product_id/RJ1.html")
        );
        assert_eq!(shelf.page("RJ2"), None);
        let labels: Vec<String> = shelf.listing().labels.into_iter().map(|l| l.name).collect();
        assert_eq!(labels, ["お気に入り", "非表示"]);
        shelf.set_label("favorite", "RJ1", true).unwrap();

        // The next reading replaces the works; a work keeps its label and its place
        // (it was opened) by its ID.
        shelf.keep(&[
            work("RJ9", "Newer", "r15", Some(9)),
            work("RJ1", "Voice, renamed", "r18", Some(1)),
            video("RJ8", "Video", "r18", Some(0)),
        ]);
        let listing = shelf.listing();
        let rows: Vec<(&str, &str, &[String])> = listing
            .items
            .iter()
            .map(|i| (i.id.as_str(), i.name.as_str(), i.labels.as_slice()))
            .collect();
        assert_eq!(
            rows,
            [
                ("RJ1", "Voice, renamed", &["favorite".to_owned()][..]),
                ("RJ9", "Newer", &[][..]),
                ("RJ8", "Video", &[][..]),
            ]
        );

        // After a restart the works are there before anything is read, whole: the
        // video is still one to download.
        let again = self::shelf(Works::R18, store, session("again"));
        assert_eq!(again.listing().items, listing.items);
        assert!(!again.listing().items[2].installed);
    }

    #[test]
    fn the_works_stay_while_the_purchases_cannot_be_read_and_the_round_says_why() {
        let session = session("round");
        let wake = Arc::new(tokio::sync::Notify::new());
        let (_, videos) = videos("round", &session);
        let shelf = Shelf::new(
            Works::AllAges,
            store(Works::AllAges),
            session.clone(),
            wake.clone(),
            videos,
        )
        .unwrap();
        shelf.keep(&[work("RJ1", "Voice", "all", Some(1))]);

        shelf.begin_round();
        let round = shelf.round().unwrap();
        assert_eq!(round.doing, Some(Doing::Purchases));
        assert!(round.started.is_some());
        shelf.refresh(&Err(
            "DLsite needs signing in again through the panel".into()
        ));
        shelf.end_round();
        let round = shelf.round().unwrap();
        assert_eq!(round.doing, None);
        assert!(round.finished.is_some());
        assert_eq!(
            round.purchases.as_deref(),
            Some("DLsite needs signing in again through the panel")
        );
        assert_eq!(round.jobs, []);
        assert_eq!(ids(&shelf), ["RJ1"]);

        // A reading that works brings the shelf up to date.
        shelf.begin_round();
        shelf.refresh(&Ok(vec![work("RJ2", "Another", "all", Some(2))]));
        assert_eq!(shelf.round().unwrap().purchases, None);
        assert_eq!(ids(&shelf), ["RJ2"]);

        // Updating reads the purchases again, once the user has signed in.
        assert_eq!(shelf.update(&[]), Err(UpdateError::SignIn));
        assert!(wake.notified().now_or_never().is_none());
        let cookie = crate::shop::cookies::Cookie {
            name: "__DLsite_SID".into(),
            value: "v".into(),
            domain: ".dlsite.com".into(),
            path: "/".into(),
            expires: None,
            secure: true,
            http_only: true,
        };
        session.keep(vec![cookie]).unwrap();
        assert_eq!(shelf.listing().sign_in, None);
        assert_eq!(shelf.update(&[]), Ok(Update::Round));
        assert!(wake.notified().now_or_never().is_some());
    }
}
