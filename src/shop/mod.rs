//! A shop's game library read from the folders `<root>\<maker>\<title>`: the games bought
//! but not there yet are downloaded into the same layout, and updated games are brought
//! up to date. Which work each folder is, labels, the chosen programs and when each game
//! was last started are kept in windows-link's database, in tables of the shop's own. A
//! game goes by its work ID once known, else by an ID from its folder. The shop itself
//! (signing in, reading purchases, fetching files) is behind `Shop`.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::library::{
    GameLibrary, Item, KeyError, Label, LabelError, LicenseKey, Listing, Picture, Programs, Start,
    StartError, Update, UpdateError,
};

pub mod download;

/// Labels every shop's library has, like Steam's own.
const FIXED_LABELS: [(&str, &str); 2] = [("favorite", "お気に入り"), ("hidden", "非表示")];

/// A game's folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Title {
    pub id: String,
    pub maker: String,
    pub name: String,
    pub folder: PathBuf,
    pub modified: SystemTime,
}

/// How a folder is recorded: its path in lower case, as DLsiteNest records it too.
pub(crate) fn folder_key(folder: &Path) -> String {
    folder.to_string_lossy().to_lowercase()
}

/// A stable ID for a game from where it lives, safe in a URL.
pub fn title_id(maker: &str, name: &str) -> String {
    let digest = Sha256::digest(format!("{maker}/{name}").as_bytes());
    digest[..8].iter().fold(String::new(), |mut id, b| {
        let _ = write!(id, "{b:02x}");
        id
    })
}

/// The file name without `.exe`, in lower case.
fn stem(file: &str) -> String {
    let file = file.rsplit('/').next().unwrap_or(file).to_lowercase();
    file.strip_suffix(".exe").map(str::to_owned).unwrap_or(file)
}

/// Programs that come with a game but never start it.
fn is_helper(file: &str) -> bool {
    let stem = stem(file);
    matches!(
        stem.as_str(),
        "unitycrashhandler32"
            | "unitycrashhandler64"
            | "notification_helper"
            | "crashpad_handler"
            | "dxsetup"
            | "dxwebsetup"
            | "oalinst"
    ) || [
        "unins",
        "uninstall",
        "vcredist",
        "vc_redist",
        "ue4prereqsetup",
        "dotnet",
    ]
    .iter()
    .any(|p| stem.starts_with(p))
        || stem.contains("ファイル破損チェック")
}

/// Programs that set a game up rather than play it.
fn is_tool(file: &str) -> bool {
    let stem = stem(file);
    [
        "config",
        "setting",
        "setup",
        "patch",
        "install",
        "launcher",
        "設定",
        "インストーラ",
    ]
    .iter()
    .any(|word| stem.contains(word))
}

fn exes(files: &[String]) -> impl Iterator<Item = &String> {
    files
        .iter()
        .filter(|f| f.to_lowercase().ends_with(".exe") && !is_helper(f))
}

/// The programs that may start a game: the `.exe` files in its folder, or else in the
/// folders right below it (as `folder/file.exe`), leaving out helpers such as crash
/// reporters and uninstallers.
pub fn candidates(top: &[String], below: &[(String, Vec<String>)]) -> Vec<String> {
    let here: Vec<String> = exes(top).cloned().collect();
    if !here.is_empty() {
        return here;
    }
    below
        .iter()
        .flat_map(|(folder, files)| exes(files).map(move |f| format!("{folder}/{f}")))
        .collect()
}

/// The program to start when the user has not chosen one: the only candidate, or the
/// only one that is not a tool (settings, setup, patcher, launcher). `None` when the
/// user has to choose.
pub fn default_program(candidates: &[String]) -> Option<&String> {
    if let [only] = candidates {
        return Some(only);
    }
    let mut games = candidates.iter().filter(|c| !is_tool(c));
    match (games.next(), games.next()) {
        (Some(game), None) => Some(game),
        _ => None,
    }
}

/// Which work each folder is, labels, chosen programs and start times, in windows-link's
/// database. Labels, programs and start times are kept by the game's ID.
pub struct ShopStore {
    conn: Mutex<Connection>,
    /// The shop's tables are named `<prefix>_…`, such as `dlsite_games`.
    prefix: &'static str,
}

impl ShopStore {
    pub fn open(path: &Path, prefix: &'static str) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?, prefix)
    }

    pub fn in_memory(prefix: &'static str) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, prefix)
    }

    /// SQL for this shop's tables, written with `shop_` for the prefix.
    fn sql(&self, text: &str) -> String {
        text.replace("shop_", &format!("{}_", self.prefix))
    }

    fn init(conn: Connection, prefix: &'static str) -> rusqlite::Result<Self> {
        let store = Self {
            conn: Mutex::new(conn),
            prefix,
        };
        store.create()?;
        Ok(store)
    }

    fn create(&self) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute_batch(&self.sql(
            "CREATE TABLE IF NOT EXISTS shop_labels (
                 id       TEXT PRIMARY KEY,
                 name     TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shop_label_items (
                 label_id TEXT NOT NULL,
                 item_id  TEXT NOT NULL,
                 PRIMARY KEY (label_id, item_id)
             );
             CREATE TABLE IF NOT EXISTS shop_programs (
                 item_id TEXT PRIMARY KEY,
                 program TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shop_started (
                 item_id TEXT PRIMARY KEY,
                 at      INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shop_games (
                 work_id TEXT PRIMARY KEY,
                 path    TEXT NOT NULL UNIQUE,
                 image   TEXT,
                 version TEXT
             );",
        ))?;
        // The previous version's table has no version yet.
        let versioned: bool = conn.query_row(
            &self.sql("SELECT EXISTS (SELECT 1 FROM pragma_table_info('shop_games') WHERE name = 'version')"),
            [],
            |row| row.get(0),
        )?;
        if !versioned {
            conn.execute_batch(&self.sql("ALTER TABLE shop_games ADD COLUMN version TEXT"))?;
        }
        for (position, (id, name)) in FIXED_LABELS.iter().enumerate() {
            conn.execute(
                &self.sql(
                    "INSERT OR IGNORE INTO shop_labels (id, name, position) VALUES (?1, ?2, ?3)",
                ),
                params![id, name, i64::try_from(position).unwrap_or(0)],
            )?;
        }
        Ok(())
    }

    pub(crate) fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn labels(&self) -> rusqlite::Result<Vec<(Label, HashSet<String>)>> {
        let conn = self.conn();
        let mut items: HashMap<String, HashSet<String>> = HashMap::new();
        let mut stmt = conn.prepare(&self.sql("SELECT label_id, item_id FROM shop_label_items"))?;
        for row in stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))? {
            let (label, item) = row?;
            items.entry(label).or_default().insert(item);
        }
        let mut stmt =
            conn.prepare(&self.sql("SELECT id, name FROM shop_labels ORDER BY position"))?;
        let labels = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))?;
        labels
            .map(|row| {
                let (id, name): (String, String) = row?;
                let editable = !FIXED_LABELS.iter().any(|(fixed, _)| *fixed == id);
                let members = items.remove(&id).unwrap_or_default();
                Ok((Label { id, name, editable }, members))
            })
            .collect()
    }

    pub fn create_label(&self, name: &str) -> rusqlite::Result<Label> {
        let conn = self.conn();
        let next: i64 = conn.query_row(
            &self.sql("SELECT COALESCE(MAX(position), 0) + 1 FROM shop_labels"),
            [],
            |row| row.get(0),
        )?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let id = format!("{}-{nanos:x}", self.prefix);
        conn.execute(
            &self.sql("INSERT INTO shop_labels (id, name, position) VALUES (?1, ?2, ?3)"),
            params![id, name, next],
        )?;
        Ok(Label {
            id,
            name: name.to_owned(),
            editable: true,
        })
    }

    /// Whether the label was there.
    pub fn rename_label(&self, id: &str, name: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            &self.sql("UPDATE shop_labels SET name = ?2 WHERE id = ?1"),
            params![id, name],
        )?;
        Ok(changed > 0)
    }

    /// Whether the label was there.
    pub fn delete_label(&self, id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        conn.execute(
            &self.sql("DELETE FROM shop_label_items WHERE label_id = ?1"),
            params![id],
        )?;
        Ok(conn.execute(
            &self.sql("DELETE FROM shop_labels WHERE id = ?1"),
            params![id],
        )? > 0)
    }

    /// Whether the label exists.
    pub fn set_label(&self, label: &str, item: &str, on: bool) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            &self.sql("SELECT EXISTS (SELECT 1 FROM shop_labels WHERE id = ?1)"),
            params![label],
            |row| row.get(0),
        )?;
        if exists {
            let sql = if on {
                &self.sql(
                    "INSERT OR IGNORE INTO shop_label_items (label_id, item_id) VALUES (?1, ?2)",
                )
            } else {
                &self.sql("DELETE FROM shop_label_items WHERE label_id = ?1 AND item_id = ?2")
            };
            conn.execute(sql, params![label, item])?;
        }
        Ok(exists)
    }

    pub fn programs(&self) -> rusqlite::Result<HashMap<String, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&self.sql("SELECT item_id, program FROM shop_programs"))?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn choose(&self, item: &str, program: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            &self.sql(
                "INSERT INTO shop_programs (item_id, program) VALUES (?1, ?2)
             ON CONFLICT (item_id) DO UPDATE SET program = excluded.program",
            ),
            params![item, program],
        )?;
        Ok(())
    }

    pub fn started(&self) -> rusqlite::Result<HashMap<String, i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&self.sql("SELECT item_id, at FROM shop_started"))?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Which work each folder is, by `folder_key`. Works whose folder is gone
    /// stay, so a game downloaded again gets its labels back.
    pub fn games(&self) -> rusqlite::Result<HashMap<String, Known>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&self.sql("SELECT path, work_id, image FROM shop_games"))?;
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
    }

    /// Record the work of each title in `known` (by title ID) with its folder, the first
    /// of `titles` when several share a work, keeping the picture known before when there
    /// is no new one, and move what was kept under the folder's ID (labels, program,
    /// start time, pins) to the work's.
    #[allow(clippy::implicit_hasher, reason = "built by identify")]
    pub fn remember(
        &self,
        titles: &[Title],
        known: &HashMap<String, Known>,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let saving = conn.transaction()?;
        let pins: bool = saving.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'library_pins')",
            [],
            |row| row.get(0),
        )?;
        let mut seen = HashSet::new();
        for title in titles {
            let Some(game) = known.get(&title.id) else {
                continue;
            };
            if !seen.insert(game.work.as_str()) {
                continue;
            }
            let path = folder_key(&title.folder);
            saving.execute(
                &self.sql("DELETE FROM shop_games WHERE path = ?1 AND work_id <> ?2"),
                params![path, game.work],
            )?;
            saving.execute(
                &self.sql(
                    "INSERT INTO shop_games (work_id, path, image) VALUES (?1, ?2, ?3)
                 ON CONFLICT (work_id) DO UPDATE SET
                     path = excluded.path,
                     image = COALESCE(excluded.image, shop_games.image)",
                ),
                params![game.work, path, game.image],
            )?;
            let mut tables = vec![
                self.sql("shop_label_items"),
                self.sql("shop_programs"),
                self.sql("shop_started"),
            ];
            if pins {
                tables.push("library_pins".to_owned());
            }
            for table in tables {
                saving.execute(
                    &format!("UPDATE OR IGNORE {table} SET item_id = ?2 WHERE item_id = ?1"),
                    params![title.id, game.work],
                )?;
                saving.execute(
                    &format!("DELETE FROM {table} WHERE item_id = ?1"),
                    params![title.id],
                )?;
            }
        }
        saving.commit()
    }

    /// The version of each game windows-link downloaded or found current, by work ID.
    pub fn versions(&self) -> rusqlite::Result<HashMap<String, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            &self.sql("SELECT work_id, version FROM shop_games WHERE version IS NOT NULL"),
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Remember that a game's folder holds `version`.
    pub fn set_version(&self, work: &str, version: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            &self.sql("UPDATE shop_games SET version = ?2 WHERE work_id = ?1"),
            params![work, version],
        )?;
        Ok(())
    }

    /// Record a work downloaded into `folder` at `version`, keeping the picture known
    /// before when there is no new one.
    pub fn record_download(
        &self,
        work: &str,
        folder: &Path,
        image: Option<&str>,
        version: &str,
    ) -> rusqlite::Result<()> {
        let path = folder_key(folder);
        let conn = self.conn();
        conn.execute(
            &self.sql("DELETE FROM shop_games WHERE path = ?1 AND work_id <> ?2"),
            params![path, work],
        )?;
        conn.execute(
            &self.sql(
                "INSERT INTO shop_games (work_id, path, image, version) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (work_id) DO UPDATE SET
                 path = excluded.path,
                 image = COALESCE(excluded.image, shop_games.image),
                 version = excluded.version",
            ),
            params![work, path, image, version],
        )?;
        Ok(())
    }

    /// Record a start, ordered after every earlier one.
    pub fn mark_started(&self, item: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            &self.sql(
                "INSERT INTO shop_started (item_id, at)
             VALUES (?1, (SELECT COALESCE(MAX(at), 0) + 1 FROM shop_started))
             ON CONFLICT (item_id) DO UPDATE SET at = excluded.at",
            ),
            params![item],
        )?;
        Ok(())
    }
}

fn file_names(dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => folders.push(name),
            Ok(_) => files.push(name),
            Err(_) => {}
        }
    }
    files.sort();
    folders.sort();
    (files, folders)
}

/// The games under `root` with their program candidates.
pub fn scan(root: &Path) -> Result<Vec<(Title, Vec<String>)>, String> {
    if !root.is_dir() {
        return Err(format!("{} does not exist", root.display()));
    }
    let mut games = Vec::new();
    let (_, makers) = file_names(root);
    // `.windows-link` holds downloads in progress.
    for maker in makers.into_iter().filter(|maker| !maker.starts_with('.')) {
        let (_, names) = file_names(&root.join(&maker));
        for name in names {
            // DLsiteNest keeps the previous copy of an updated game as `<title>.bak`.
            if name.to_lowercase().ends_with(".bak") {
                continue;
            }
            let folder = root.join(&maker).join(&name);
            let (top, below) = file_names(&folder);
            let below: Vec<(String, Vec<String>)> =
                if top.iter().any(|f| f.to_lowercase().ends_with(".exe")) {
                    Vec::new()
                } else {
                    below
                        .into_iter()
                        .map(|sub| {
                            let files = file_names(&folder.join(&sub)).0;
                            (sub, files)
                        })
                        .collect()
                };
            let programs = candidates(&top, &below);
            let modified = std::fs::metadata(&folder)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            games.push((
                Title {
                    id: title_id(&maker, &name),
                    maker: maker.clone(),
                    name,
                    folder,
                    modified,
                },
                programs,
            ));
        }
    }
    Ok(games)
}

/// A program path relative to the game's folder, as a path on this PC.
fn program_path(folder: &Path, program: &str) -> PathBuf {
    program
        .split('/')
        .fold(folder.to_path_buf(), |path, part| path.join(part))
}

/// The status of a game waiting to be downloaded.
const WAITING: &str = "ダウンロード待ち";

/// The work a game folder is, with its main picture when known.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub work: String,
    pub image: Option<String>,
}

/// The games bought but not in the folder yet (by work ID, with the shop's name, maker
/// and picture), and how getting each is going.
#[allow(clippy::implicit_hasher, reason = "built by the library")]
pub fn waiting(
    purchases: &[Work],
    here: &HashSet<String>,
    labels: &[(Label, HashSet<String>)],
    progress: &HashMap<String, String>,
) -> Vec<Item> {
    purchases
        .iter()
        .filter(|work| !here.contains(&work.id))
        .map(|work| Item {
            id: work.id.clone(),
            name: work.name.clone(),
            detail: Some(work.maker.clone()),
            choosable: false,
            image: work.image.clone(),
            installed: false,
            labels: labels
                .iter()
                .filter(|(_, items)| items.contains(&work.id))
                .map(|(label, _)| label.id.clone())
                .collect(),
            status: Some(
                progress
                    .get(&work.id)
                    .cloned()
                    .unwrap_or_else(|| WAITING.to_owned()),
            ),
        })
        .collect()
}

/// The listing: most recently started first, then most recently installed. `images`
/// are the shop's pictures by game ID.
#[allow(clippy::implicit_hasher, reason = "the maps come from the store")]
pub fn listing(
    titles: &[(Title, Vec<String>)],
    labels: &[(Label, HashSet<String>)],
    started: &HashMap<String, i64>,
    images: &HashMap<String, String>,
) -> Listing {
    let mut order: Vec<&(Title, Vec<String>)> = titles.iter().collect();
    order.sort_by(|(a, _), (b, _)| {
        let last = |t: &Title| started.get(&t.id).copied().unwrap_or(i64::MIN);
        last(b)
            .cmp(&last(a))
            .then_with(|| b.modified.cmp(&a.modified))
            .then_with(|| a.name.cmp(&b.name))
    });
    Listing {
        items: order
            .into_iter()
            .map(|(title, programs)| Item {
                id: title.id.clone(),
                name: title.name.clone(),
                detail: Some(title.maker.clone()),
                choosable: programs.len() > 1,
                image: images.get(&title.id).cloned(),
                installed: true,
                status: None,
                labels: labels
                    .iter()
                    .filter(|(_, items)| items.contains(&title.id))
                    .map(|(label, _)| label.id.clone())
                    .collect(),
            })
            .collect(),
        labels: labels.iter().map(|(label, _)| label.clone()).collect(),
        partial: None,
        labels_locked: None,
        sign_in: None,
    }
}

fn stored<T>(result: rusqlite::Result<T>) -> Result<T, LabelError> {
    result.map_err(|err| LabelError::Failed(err.to_string()))
}

/// Refuse to rename or delete the labels every library has.
fn fixed(label: &str) -> Result<(), LabelError> {
    if FIXED_LABELS.iter().any(|(id, _)| *id == label) {
        return Err(LabelError::Invalid(format!(
            "{label:?} cannot be renamed or deleted"
        )));
    }
    Ok(())
}

/// A purchased work.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Work {
    /// The shop's ID, such as DLsite's `RJ01464588`.
    pub id: String,
    pub name: String,
    pub maker: String,
    /// The main picture's URL.
    pub image: Option<String>,
    /// The shop's work type, such as DLsite's `RPG` or `SOU` (voice).
    pub kind: String,
    /// Whether it runs on Windows.
    pub windows: bool,
    /// When its latest version came out (its last update, else its release), as the
    /// shop writes it.
    pub version: String,
}

/// What a library needs from the shop its games come from.
pub trait Shop: Send + Sync + 'static {
    /// The shop's name, for logs and messages.
    fn name(&self) -> &'static str;
    /// Whether a purchase is a game this library downloads and lists.
    fn wants(&self, work: &Work) -> bool;
    /// Whether downloads can go ahead (the shop can be signed in to).
    fn ready(&self) -> bool;
    /// When a version (as the shop writes it) came out, for a folder windows-link did
    /// not fill.
    fn released(&self, version: &str) -> Option<SystemTime>;
    /// Download a work's files into `staging`, going on from parts already there.
    /// `progress` hears the bytes of the file being fetched and its size. The first
    /// file returned is the one to unpack.
    fn fetch(
        &self,
        work: &Work,
        staging: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<PathBuf>, String>;
    /// A work's license keys, read from the shop each time.
    fn license_keys(&self, work: &str) -> Result<Vec<LicenseKey>, KeyError>;
    /// The shop the user has to sign in to through the panel, when it is so.
    fn sign_in(&self) -> Option<&'static str> {
        None
    }
}

/// A shop's games under one folder, `<root>\<maker>\<title>`.
pub struct ShopLibrary<S> {
    pub(crate) root: PathBuf,
    pub(crate) store: std::sync::Arc<ShopStore>,
    shop: S,
    /// The games bought, as last read.
    pub(crate) purchases: Mutex<Vec<Work>>,
    /// How each download or update planned this round is going, by work ID.
    pub(crate) progress: Mutex<HashMap<String, String>>,
    /// Wakes the round of downloads and updates before its time.
    wake: std::sync::Arc<tokio::sync::Notify>,
}

impl<S: Shop> ShopLibrary<S> {
    pub fn new(root: PathBuf, store: std::sync::Arc<ShopStore>, shop: S) -> Self {
        Self {
            root,
            store,
            shop,
            purchases: Mutex::default(),
            progress: Mutex::default(),
            wake: std::sync::Arc::default(),
        }
    }

    /// Share `wake` with the round of downloads and updates, which `update` wakes.
    #[must_use]
    pub fn with_wake(mut self, wake: std::sync::Arc<tokio::sync::Notify>) -> Self {
        self.wake = wake;
        self
    }

    pub fn shop(&self) -> &S {
        &self.shop
    }

    /// Keep the purchases just read, the games among them.
    pub fn set_purchases(&self, works: &[Work]) {
        *self
            .purchases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = works
            .iter()
            .filter(|work| self.shop.wants(work))
            .cloned()
            .collect();
    }

    fn purchases(&self) -> Vec<Work> {
        self.purchases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Download the games bought but not in the folder yet, and update those whose
    /// folder is older than their latest version, one at a time. Needs the purchases
    /// read before. Failures are logged and shown in the listing, and tried again
    /// next time.
    pub fn download(&self) {
        if !self.shop.ready() {
            return;
        }
        let purchases = self.purchases();
        let Ok(games) = scan(&self.root) else {
            return;
        };
        let records = self.store.games().unwrap_or_default();
        let versions = self.store.versions().unwrap_or_default();
        let on_disk: HashMap<String, download::OnDisk> = games
            .iter()
            .filter_map(|(title, _)| {
                let work = &records.get(&folder_key(&title.folder))?.work;
                let disk = download::OnDisk {
                    folder: title.folder.clone(),
                    version: versions.get(work).cloned(),
                    modified: title.modified,
                };
                Some((work.clone(), disk))
            })
            .collect();
        let plan = download::plan(&purchases, &on_disk, &|version| self.shop.released(version));
        for (work, version) in &plan.current {
            let _ = self.store.set_version(&work.id, version);
        }
        {
            let mut progress = self.progress();
            progress.clear();
            for work in &plan.fresh {
                progress.insert(work.id.clone(), WAITING.to_owned());
            }
            for (work, _) in &plan.updates {
                progress.insert(work.id.clone(), "更新待ち".to_owned());
            }
        }
        let shop = self.shop.name();
        tracing::info!(
            shop,
            new = plan.fresh.len(),
            updates = plan.updates.len(),
            "downloads planned"
        );
        let jobs = plan.fresh.iter().map(|work| (*work, None)).chain(
            plan.updates
                .iter()
                .map(|(work, folder)| (*work, Some(folder.as_path()))),
        );
        let (mut done, mut failed) = (0, 0);
        for (work, folder) in jobs {
            match self.download_one(work, folder) {
                Ok(folder) => {
                    done += 1;
                    self.progress().remove(&work.id);
                    tracing::info!(shop, work = %work.id, folder = %folder.display(), "game downloaded");
                }
                Err(reason) => {
                    failed += 1;
                    let verb = if folder.is_some() {
                        "更新"
                    } else {
                        "ダウンロード"
                    };
                    self.progress()
                        .insert(work.id.clone(), format!("{verb}に失敗: {reason}"));
                    tracing::warn!(shop, work = %work.id, %reason, "cannot download a game");
                }
            }
        }
        tracing::info!(shop, done, failed, "downloads finished");
    }

    /// Download one work into a new folder, or over `folder` to update it. The files
    /// wait in `<root>\.windows-link\<ID>` until they are in place, so a stopped
    /// download goes on next time.
    fn download_one(&self, work: &Work, folder: Option<&Path>) -> Result<PathBuf, String> {
        let verb = if folder.is_some() {
            "更新"
        } else {
            "ダウンロード"
        };
        let staging = self.root.join(".windows-link").join(&work.id);
        std::fs::create_dir_all(&staging).map_err(|err| format!("{}: {err}", staging.display()))?;
        let files = self.shop.fetch(work, &staging, &mut |have, total| {
            let percent = have * 100 / total.max(1);
            self.progress()
                .insert(work.id.clone(), format!("{verb}中 {percent}%"));
        })?;
        self.progress().insert(work.id.clone(), "展開中".to_owned());
        let out = staging.join("out");
        let _ = std::fs::remove_dir_all(&out);
        let first = files
            .first()
            .ok_or_else(|| format!("{} gave no files", self.shop.name()))?;
        let placed = download::unpack(first, &out).and_then(|()| {
            let target = folder.map_or_else(|| self.new_folder(work), Path::to_path_buf);
            download::place(&download::content_root(&out, &target), &target)
                .map_err(|err| format!("{}: {err}", target.display()))?;
            Ok(target)
        });
        match placed {
            Ok(target) => {
                self.store
                    .record_download(&work.id, &target, work.image.as_deref(), &work.version)
                    .map_err(|err| err.to_string())?;
                let _ = std::fs::remove_dir_all(&staging);
                Ok(target)
            }
            Err(reason) => {
                let _ = std::fs::remove_dir_all(&out);
                Err(reason)
            }
        }
    }

    /// Where a new game goes: `<root>\<maker>\<title>`, or with its work ID after the
    /// title when another game has that folder.
    fn new_folder(&self, work: &Work) -> PathBuf {
        let maker = self.root.join(download::folder_name(&work.maker));
        let title = download::folder_name(&work.name);
        let folder = maker.join(&title);
        if folder.exists() {
            maker.join(format!("{title} ({})", work.id))
        } else {
            folder
        }
    }

    /// Why a game bought but not here cannot start yet, or `None` when `id` is not one.
    fn not_downloaded(&self, id: &str) -> Option<String> {
        let bought = self
            .purchases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|work| work.id == id);
        bought.then(|| {
            self.progress()
                .get(id)
                .cloned()
                .unwrap_or_else(|| WAITING.to_owned())
        })
    }

    /// The games, each by its work ID when known.
    pub(crate) fn games(&self) -> Result<Vec<(Title, Vec<String>)>, String> {
        let mut games = scan(&self.root)?;
        let records = self.store.games().unwrap_or_default();
        for (title, _) in &mut games {
            if let Some(known) = records.get(&folder_key(&title.folder)) {
                title.id.clone_from(&known.work);
            }
        }
        Ok(games)
    }

    fn find(&self, id: &str) -> Option<(Title, Vec<String>)> {
        self.games().ok()?.into_iter().find(|(t, _)| t.id == id)
    }

    /// The program in use: the user's choice while it is still there, or the default.
    fn program(&self, id: &str, candidates: &[String]) -> Option<String> {
        let chosen = self
            .store
            .programs()
            .ok()
            .and_then(|mut all| all.remove(id));
        chosen
            .filter(|c| candidates.contains(c))
            .or_else(|| default_program(candidates).cloned())
    }
}

impl<S: Shop> GameLibrary for ShopLibrary<S> {
    fn listing(&self) -> Listing {
        let games = match self.games() {
            Ok(games) => games,
            Err(reason) => {
                return Listing {
                    partial: Some(reason),
                    labels: self
                        .store
                        .labels()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(l, _)| l)
                        .collect(),
                    sign_in: self.shop.sign_in().map(str::to_owned),
                    ..Listing::default()
                };
            }
        };
        let labels = self.store.labels().unwrap_or_default();
        let started = self.store.started().unwrap_or_default();
        let images: HashMap<String, String> = self
            .store
            .games()
            .unwrap_or_default()
            .into_values()
            .filter_map(|k| Some((k.work, k.image?)))
            .collect();
        let mut listing = listing(&games, &labels, &started, &images);
        if games.is_empty() {
            listing.partial = Some(format!("there are no games in {}", self.root.display()));
        }
        let progress = self.progress().clone();
        for item in &mut listing.items {
            item.status = progress.get(&item.id).cloned();
        }
        let here: HashSet<String> = listing.items.iter().map(|i| i.id.clone()).collect();
        let purchases = self.purchases();
        let coming = waiting(&purchases, &here, &labels, &progress);
        listing.items.splice(0..0, coming);
        listing.sign_in = self.shop.sign_in().map(str::to_owned);
        listing
    }

    fn start(&self, id: &str) -> Result<Start, StartError> {
        let Some((title, candidates)) = self.find(id) else {
            return Err(self
                .not_downloaded(id)
                .map_or(StartError::NotFound, StartError::NotDownloaded));
        };
        if candidates.is_empty() {
            return Err(StartError::NoProgram);
        }
        let program = self.program(id, &candidates).ok_or(StartError::Choose)?;
        let _ = self.store.mark_started(id);
        Ok(Start {
            open: program_path(&title.folder, &program)
                .to_string_lossy()
                .into_owned(),
            folder: Some(title.folder),
        })
    }

    fn picture(&self, id: &str) -> Option<Picture> {
        let known = self.store.games().ok().and_then(|all| {
            all.into_values()
                .find(|k| k.work == id)
                .and_then(|k| k.image)
        });
        if let Some(image) = known {
            return Some(Picture::Url(image));
        }
        let (title, candidates) = self.find(id)?;
        let program = self
            .program(id, &candidates)
            .or_else(|| candidates.first().cloned())?;
        Some(Picture::Icon(program_path(&title.folder, &program)))
    }

    fn folder(&self, id: &str) -> Option<PathBuf> {
        self.find(id).map(|(title, _)| title.folder)
    }

    fn create_label(&self, name: &str) -> Result<Label, LabelError> {
        stored(self.store.create_label(name))
    }

    fn rename_label(&self, label: &str, name: &str) -> Result<(), LabelError> {
        fixed(label)?;
        stored(self.store.rename_label(label, name))?
            .then_some(())
            .ok_or(LabelError::NotFound)
    }

    fn delete_label(&self, label: &str) -> Result<(), LabelError> {
        fixed(label)?;
        stored(self.store.delete_label(label))?
            .then_some(())
            .ok_or(LabelError::NotFound)
    }

    fn set_label(&self, label: &str, item: &str, on: bool) -> Result<(), LabelError> {
        stored(self.store.set_label(label, item, on))?
            .then_some(())
            .ok_or(LabelError::NotFound)
    }

    fn programs(&self, id: &str) -> Option<Programs> {
        let (_, candidates) = self.find(id)?;
        let chosen = self.program(id, &candidates);
        Some(Programs { candidates, chosen })
    }

    fn license_keys(&self, id: &str) -> Result<Vec<LicenseKey>, KeyError> {
        let bought = self
            .purchases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|work| work.id == id);
        if !bought {
            return Err(KeyError::NotFound);
        }
        self.shop.license_keys(id)
    }

    fn sign_in(&self) -> Option<&'static str> {
        self.shop.sign_in()
    }

    /// Start the round of downloads and updates now rather than at its time, or right
    /// after the one under way. Every game bought is downloaded, as in each round.
    fn update(&self, _hide: &[String]) -> Result<Update, UpdateError> {
        if self.shop.sign_in().is_some() {
            return Err(UpdateError::SignIn);
        }
        if !self.shop.ready() {
            return Err(UpdateError::Unavailable(format!(
                "{} cannot download games now",
                self.shop.name()
            )));
        }
        self.wake.notify_one();
        Ok(Update::Round)
    }

    fn choose_program(&self, id: &str, program: &str) -> Result<(), LabelError> {
        let (_, candidates) = self.find(id).ok_or(LabelError::NotFound)?;
        if !candidates.iter().any(|c| c == program) {
            return Err(LabelError::Invalid(format!(
                "{program:?} is not one of the game's programs"
            )));
        }
        stored(self.store.choose(id, program))
    }
}
