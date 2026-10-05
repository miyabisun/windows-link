//! A DLsite game library read from the folders DLsiteNest makes, `<root>\<maker>\<title>`,
//! so it keeps working without DLsiteNest. Labels, the chosen programs and when each
//! game was last started are kept in windows-link's database.

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
    GameLibrary, Item, Label, LabelError, Listing, Picture, Programs, Start, StartError,
};

pub mod nest;
pub mod play;

/// Where DLsiteNest puts games.
pub const DEFAULT_ROOT: &str = r"D:\DLsiteNest\Game";

/// Labels every DLsite library has, like Steam's own.
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

/// Labels, chosen programs and start times, in windows-link's database.
pub struct DlsiteStore {
    conn: Mutex<Connection>,
}

impl DlsiteStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dlsite_labels (
                 id       TEXT PRIMARY KEY,
                 name     TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS dlsite_label_items (
                 label_id TEXT NOT NULL,
                 item_id  TEXT NOT NULL,
                 PRIMARY KEY (label_id, item_id)
             );
             CREATE TABLE IF NOT EXISTS dlsite_programs (
                 item_id TEXT PRIMARY KEY,
                 program TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS dlsite_started (
                 item_id TEXT PRIMARY KEY,
                 at      INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS dlsite_works (
                 item_id TEXT PRIMARY KEY,
                 work_id TEXT NOT NULL,
                 image   TEXT
             );",
        )?;
        for (position, (id, name)) in FIXED_LABELS.iter().enumerate() {
            conn.execute(
                "INSERT OR IGNORE INTO dlsite_labels (id, name, position) VALUES (?1, ?2, ?3)",
                params![id, name, i64::try_from(position).unwrap_or(0)],
            )?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn labels(&self) -> rusqlite::Result<Vec<(Label, HashSet<String>)>> {
        let conn = self.conn();
        let mut items: HashMap<String, HashSet<String>> = HashMap::new();
        let mut stmt = conn.prepare("SELECT label_id, item_id FROM dlsite_label_items")?;
        for row in stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))? {
            let (label, item) = row?;
            items.entry(label).or_default().insert(item);
        }
        let mut stmt = conn.prepare("SELECT id, name FROM dlsite_labels ORDER BY position")?;
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
            "SELECT COALESCE(MAX(position), 0) + 1 FROM dlsite_labels",
            [],
            |row| row.get(0),
        )?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let id = format!("dl-{nanos:x}");
        conn.execute(
            "INSERT INTO dlsite_labels (id, name, position) VALUES (?1, ?2, ?3)",
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
            "UPDATE dlsite_labels SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(changed > 0)
    }

    /// Whether the label was there.
    pub fn delete_label(&self, id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM dlsite_label_items WHERE label_id = ?1",
            params![id],
        )?;
        Ok(conn.execute("DELETE FROM dlsite_labels WHERE id = ?1", params![id])? > 0)
    }

    /// Whether the label exists.
    pub fn set_label(&self, label: &str, item: &str, on: bool) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM dlsite_labels WHERE id = ?1)",
            params![label],
            |row| row.get(0),
        )?;
        if exists {
            let sql = if on {
                "INSERT OR IGNORE INTO dlsite_label_items (label_id, item_id) VALUES (?1, ?2)"
            } else {
                "DELETE FROM dlsite_label_items WHERE label_id = ?1 AND item_id = ?2"
            };
            conn.execute(sql, params![label, item])?;
        }
        Ok(exists)
    }

    pub fn programs(&self) -> rusqlite::Result<HashMap<String, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT item_id, program FROM dlsite_programs")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn choose(&self, item: &str, program: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO dlsite_programs (item_id, program) VALUES (?1, ?2)
             ON CONFLICT (item_id) DO UPDATE SET program = excluded.program",
            params![item, program],
        )?;
        Ok(())
    }

    pub fn started(&self) -> rusqlite::Result<HashMap<String, i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT item_id, at FROM dlsite_started")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Which DLsite work each game is, by game ID.
    pub fn works(&self) -> rusqlite::Result<HashMap<String, Known>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT item_id, work_id, image FROM dlsite_works")?;
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

    /// Add or update what is known; games not in `known` keep what was known before.
    #[allow(clippy::implicit_hasher, reason = "built by identify")]
    pub fn save_works(&self, known: &HashMap<String, Known>) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let saving = conn.transaction()?;
        for (item, work) in known {
            saving.execute(
                "INSERT INTO dlsite_works (item_id, work_id, image) VALUES (?1, ?2, ?3)
                 ON CONFLICT (item_id) DO UPDATE SET
                     work_id = excluded.work_id,
                     image = COALESCE(excluded.image, dlsite_works.image)",
                params![item, work.work, work.image],
            )?;
        }
        saving.commit()
    }

    /// Record a start, ordered after every earlier one.
    pub fn mark_started(&self, item: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO dlsite_started (item_id, at)
             VALUES (?1, (SELECT COALESCE(MAX(at), 0) + 1 FROM dlsite_started))
             ON CONFLICT (item_id) DO UPDATE SET at = excluded.at",
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
    for maker in makers {
        let (_, names) = file_names(&root.join(&maker));
        for name in names {
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

/// The DLsite games under one folder (`DEFAULT_ROOT` unless configured).
pub struct DlsiteLibrary {
    root: PathBuf,
    store: std::sync::Arc<DlsiteStore>,
}

impl DlsiteLibrary {
    pub fn new(root: PathBuf, store: std::sync::Arc<DlsiteStore>) -> Self {
        Self { root, store }
    }

    /// Find out again which DLsite work each game is: from DLsiteNest's records, and
    /// from the account's purchases when `secrets.yaml` has one. Pictures of works known
    /// only through DLsiteNest come from DLsite's public information. Failures are
    /// logged and leave what was known.
    pub fn refresh(&self, account: Option<&crate::secrets::DlsiteAccount>) {
        let Ok(games) = scan(&self.root) else {
            return;
        };
        let titles: Vec<Title> = games.into_iter().map(|(title, _)| title).collect();
        let nest = nest::read();
        let purchases = match account.map(play::fetch_purchases) {
            Some(Ok(works)) => {
                tracing::info!(works = works.len(), "DLsite purchases read");
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
        let known = identify(&titles, &nest, &purchases, &images);
        tracing::info!(
            games = titles.len(),
            identified = known.len(),
            "DLsite games identified"
        );
        if let Err(err) = self.store.save_works(&known) {
            tracing::warn!(%err, "cannot keep which DLsite work each game is");
        }
    }

    fn find(&self, id: &str) -> Option<(Title, Vec<String>)> {
        scan(&self.root).ok()?.into_iter().find(|(t, _)| t.id == id)
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

fn stored<T>(result: rusqlite::Result<T>) -> Result<T, LabelError> {
    result.map_err(|err| LabelError::Failed(err.to_string()))
}

impl GameLibrary for DlsiteLibrary {
    fn listing(&self) -> Listing {
        let games = match scan(&self.root) {
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
                    ..Listing::default()
                };
            }
        };
        let labels = self.store.labels().unwrap_or_default();
        let started = self.store.started().unwrap_or_default();
        let known = self.store.works().unwrap_or_default();
        let mut listing = listing(&games, &labels, &started, &known);
        if games.is_empty() {
            listing.partial = Some(format!("there are no games in {}", self.root.display()));
        }
        listing
    }

    fn start(&self, id: &str) -> Result<Start, StartError> {
        let (title, candidates) = self.find(id).ok_or(StartError::NotFound)?;
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
        let known = self.store.works().ok().and_then(|mut all| all.remove(id));
        if let Some(image) = known.and_then(|k| k.image) {
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

/// Refuse to rename or delete the labels every library has.
fn fixed(label: &str) -> Result<(), LabelError> {
    if FIXED_LABELS.iter().any(|(id, _)| *id == label) {
        return Err(LabelError::Invalid(format!(
            "{label:?} cannot be renamed or deleted"
        )));
    }
    Ok(())
}

/// The DLsite work a game folder is, with its main picture when known.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub work: String,
    pub image: Option<String>,
}

/// Which work each game is: DLsiteNest's record of its folder, or else the purchase with
/// the same maker and title, or else the only purchase with that title. Names are
/// compared by their letters and digits only (folder names lose characters Windows does
/// not allow), ignoring case and full-width letters. Pictures come from the purchases,
/// else from `images` (DLsite's public information, by work ID).
#[allow(clippy::implicit_hasher, reason = "the maps are built here")]
pub fn identify(
    titles: &[Title],
    nest: &HashMap<String, String>,
    purchases: &[play::Work],
    images: &HashMap<String, String>,
) -> HashMap<String, Known> {
    let fold = |text: &str| -> String {
        text.chars()
            .map(|c| match c {
                // Full-width ASCII to ASCII.
                '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
                _ => c,
            })
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let by_id: HashMap<&str, &play::Work> = purchases.iter().map(|w| (w.id.as_str(), w)).collect();
    let mut by_title: HashMap<String, Vec<&play::Work>> = HashMap::new();
    for work in purchases {
        by_title.entry(fold(&work.name)).or_default().push(work);
    }
    let mut known = HashMap::new();
    for title in titles {
        let recorded = nest.get(&title.folder.to_string_lossy().to_lowercase());
        let work = if let Some(id) = recorded {
            id.clone()
        } else {
            let same = by_title
                .get(&fold(&title.name))
                .map_or(&[][..], Vec::as_slice);
            let maker = fold(&title.maker);
            match same {
                [only] => only.id.clone(),
                several => match several.iter().find(|w| fold(&w.maker) == maker) {
                    Some(work) => work.id.clone(),
                    None => continue,
                },
            }
        };
        let image = by_id
            .get(work.as_str())
            .and_then(|w| w.image.clone())
            .or_else(|| images.get(&work).cloned());
        known.insert(title.id.clone(), Known { work, image });
    }
    known
}

/// The listing: most recently started first, then most recently installed.
#[allow(clippy::implicit_hasher, reason = "the maps come from the store")]
pub fn listing(
    titles: &[(Title, Vec<String>)],
    labels: &[(Label, HashSet<String>)],
    started: &HashMap<String, i64>,
    known: &HashMap<String, Known>,
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
                image: known.get(&title.id).and_then(|k| k.image.clone()),
                installed: true,
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
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use super::{
        DlsiteLibrary, DlsiteStore, Title, candidates, default_program, listing, title_id,
    };
    use crate::library::{GameLibrary, Label, LabelError, StartError};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn games_are_identified_by_dlsitenest_then_by_name() {
        use super::{Known, identify, play::Work};

        let titles = [
            title("SPLUSH WAVE", "Grand Order 戦記", 1),
            title("サークル", "雨瀬みゆを全国大会につれてって！", 1),
            title("Other", "ＦＵＬＬ　ｗｉｄｔｈ: Title", 1),
            title("Unknown", "Nobody bought this", 1),
            title("Twice", "Same Name", 1),
        ];
        let nest = HashMap::from([(
            titles[0].folder.to_string_lossy().to_lowercase(),
            "RJ01686872".to_owned(),
        )]);
        let work = |id: &str, name: &str, maker: &str, image: Option<&str>| Work {
            id: id.into(),
            name: name.into(),
            maker: maker.into(),
            image: image.map(str::to_owned),
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
        ];
        let images = HashMap::from([("RJ01686872".to_owned(), "https://img/1.jpg".to_owned())]);
        let known = identify(&titles, &nest, &purchases, &images);
        let got = |t: &Title| known.get(&t.id).cloned();
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
        assert_eq!(got(&titles[2]).map(|k| k.work), Some("RJ3".to_owned()));
        assert_eq!(got(&titles[3]), None);
        // Two purchases share the title and neither maker matches: unknown.
        assert_eq!(got(&titles[4]), None);
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
        let known = HashMap::from([(
            b.id.clone(),
            super::Known {
                work: "RJ1".into(),
                image: Some("https://img/b.jpg".into()),
            },
        )]);
        let listing = listing(&titles, &labels, &started, &known);
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
            std::sync::Arc::new(DlsiteStore::in_memory().unwrap()),
        )
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
    fn identified_games_keep_their_work_and_picture() {
        let root = games("known");
        let library = library(&root);
        let one = library
            .listing()
            .items
            .iter()
            .find(|i| i.name == "One")
            .unwrap()
            .id
            .clone();
        let known = std::collections::HashMap::from([(
            one.clone(),
            super::Known {
                work: "RJ9".into(),
                image: Some("https://img/9.jpg".into()),
            },
        )]);
        library.store.save_works(&known).unwrap();
        // Saving again without it keeps it: a failed lookup does not forget games.
        library
            .store
            .save_works(&std::collections::HashMap::new())
            .unwrap();
        assert_eq!(library.store.works().unwrap(), known);
        assert_eq!(
            library.picture(&one),
            Some(crate::library::Picture::Url("https://img/9.jpg".into()))
        );
        let item = library
            .listing()
            .items
            .into_iter()
            .find(|i| i.id == one)
            .unwrap();
        assert_eq!(item.image.as_deref(), Some("https://img/9.jpg"));
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
