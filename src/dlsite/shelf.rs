//! The works bought on DLsite that are not games, as a library: a shelf of the all-ages
//! ones, or of the rest. Nothing of theirs is on this PC: a work opens in DLsite Play,
//! in the default browser. The works are kept in windows-link's database as last read, so
//! the list shows before the purchases are read and while signed out. Labels and when
//! each work was opened are the shelf's own.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};

use rusqlite::params;
use tokio::sync::Notify;

use super::{
    session::DlsiteSession,
    works::{Works, parse_date},
};
use crate::{
    library::{
        Doing, GameLibrary, Item, Label, LabelError, Listing, Picture, Round, Start, StartError,
        Update, UpdateError,
    },
    shop::{ShopStore, Work, unix_seconds},
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

/// The listing: the works opened last first, then those bought last.
#[allow(clippy::implicit_hasher, reason = "the maps come from the store")]
pub fn listing(
    works: &[Work],
    labels: &[(Label, HashSet<String>)],
    started: &HashMap<String, i64>,
) -> Listing {
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
            .map(|work| Item {
                id: work.id.clone(),
                name: work.name.clone(),
                detail: Some(work.maker.clone()),
                choosable: false,
                image: work.image.clone(),
                installed: true,
                labels: labels
                    .iter()
                    .filter(|(_, items)| items.contains(&work.id))
                    .map(|(label, _)| label.id.clone())
                    .collect(),
                status: (!work.playable).then(|| UNPLAYABLE.to_owned()),
                updated: parse_date(&work.version).and_then(unix_seconds),
                bought: work.bought,
                page: page(work),
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
    /// The round under way, else the last one. A shelf's round only reads the purchases.
    round: Mutex<Round>,
    /// Wakes DLsite's round, which reads the purchases again.
    wake: Arc<Notify>,
}

impl Shelf {
    /// The shelf of `which` works with what `store` keeps of it.
    pub fn new(
        which: Works,
        store: Arc<ShopStore>,
        session: Arc<DlsiteSession>,
        wake: Arc<Notify>,
    ) -> rusqlite::Result<Self> {
        store.conn().execute_batch(&store.sql(
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
        Ok(Self {
            which,
            store,
            session,
            round: Mutex::default(),
            wake,
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
                             windows, version, bought, age, site, playable)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                        work.playable
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
                        playable
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
        Listing {
            sign_in: self.sign_in().map(str::to_owned),
            ..listing(&self.works(), &labels, &started)
        }
    }

    fn start(&self, id: &str) -> Result<Start, StartError> {
        let work = self.find(id).ok_or(StartError::NotFound)?;
        let open = opens(&work).ok_or(StartError::NotFound)?;
        let _ = self.store.mark_started(id);
        Ok(Start { open, folder: None })
    }

    fn picture(&self, id: &str) -> Option<Picture> {
        self.find(id)?.image.map(Picture::Url)
    }

    /// Nothing of a work is on this PC.
    fn folder(&self, _id: &str) -> Option<PathBuf> {
        None
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

    fn round(&self) -> Option<Round> {
        Some(self.round_state().clone())
    }

    fn page(&self, id: &str) -> Option<String> {
        page(&self.find(id)?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use futures_util::FutureExt;

    use super::{Shelf, listing, opens, page};
    use crate::{
        dlsite::{session::DlsiteSession, works::Works},
        library::{
            Doing, GameLibrary, Item, Label, Picture, Start, StartError, Update, UpdateError,
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
        let listing = listing(&works, &labels, &started);
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
        let shelf =
            Shelf::new(Works::R18, store.clone(), session("keeps"), Arc::default()).unwrap();
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
            ]
        );

        // After a restart the works are there before anything is read, whole.
        let again = Shelf::new(Works::R18, store, session("again"), Arc::default()).unwrap();
        assert_eq!(again.listing().items, listing.items);
    }

    #[test]
    fn the_works_stay_while_the_purchases_cannot_be_read_and_the_round_says_why() {
        let session = session("round");
        let wake = Arc::new(tokio::sync::Notify::new());
        let shelf = Shelf::new(
            Works::AllAges,
            store(Works::AllAges),
            session.clone(),
            wake.clone(),
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
