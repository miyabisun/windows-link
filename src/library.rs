//! Game libraries behind a `*.library` button: the listing a panel searches, starting a
//! game, and the games pinned to the button's tab.

use std::{collections::HashMap, path::Path, path::PathBuf, sync::Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    buttons::PressError,
    launch::{Launcher, Program},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub labels: Vec<String>,
}

/// A group of items, such as a Steam collection. Items name their labels by `id`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Label {
    pub id: String,
    pub name: String,
    /// False for the library's own labels (Steam's favorites and hidden), which hold
    /// items but cannot be renamed or deleted.
    pub editable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Listing {
    pub items: Vec<Item>,
    /// Labels in display order.
    pub labels: Vec<Label>,
    /// Why only part of the library is listed, when it is.
    pub partial: Option<String>,
    /// Why labels cannot be changed right now, when they cannot.
    pub labels_locked: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum LabelError {
    NotFound,
    /// The request is wrong, such as an empty or taken name, or a fixed label.
    Invalid(String),
    /// Labels cannot be changed right now; says why.
    Unavailable(String),
    Failed(String),
}

/// A label's new name: trimmed, not empty, and not another label's name (ignoring
/// case). `renaming` is the label being renamed.
pub fn check_name(
    name: &str,
    labels: &[Label],
    renaming: Option<&str>,
) -> Result<String, LabelError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LabelError::Invalid("the name is empty".into()));
    }
    let taken = labels
        .iter()
        .any(|l| Some(l.id.as_str()) != renaming && l.name.to_lowercase() == name.to_lowercase());
    if taken {
        return Err(LabelError::Invalid(format!(
            "a label named {name:?} exists"
        )));
    }
    Ok(name.to_owned())
}

/// What to open to start an item, and the folder its program then runs from (to bring
/// its window to the front).
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    pub open: String,
    pub folder: Option<PathBuf>,
}

/// An item's picture: a file on this PC, or where to get it on the web.
#[derive(Clone, Debug, PartialEq)]
pub enum Picture {
    File(PathBuf),
    Url(String),
}

pub trait GameLibrary: Send + Sync + 'static {
    fn listing(&self) -> Listing;
    /// `None` for an item the library does not have.
    fn start(&self, id: &str) -> Option<Start>;
    fn picture(&self, id: &str) -> Option<Picture>;
    /// The folder an installed item lives in, to show in Explorer.
    fn folder(&self, id: &str) -> Option<PathBuf>;
    fn create_label(&self, name: &str) -> Result<Label, LabelError>;
    fn rename_label(&self, label: &str, name: &str) -> Result<(), LabelError>;
    fn delete_label(&self, label: &str) -> Result<(), LabelError>;
    /// Put an item in a label, or take it out.
    fn set_label(&self, label: &str, item: &str, on: bool) -> Result<(), LabelError>;
}

/// The library before one is set up: empty, saying why.
pub struct NoLibrary(pub String);

impl GameLibrary for NoLibrary {
    fn listing(&self) -> Listing {
        Listing {
            partial: Some(self.0.clone()),
            ..Listing::default()
        }
    }

    fn start(&self, _id: &str) -> Option<Start> {
        None
    }

    fn picture(&self, _id: &str) -> Option<Picture> {
        None
    }

    fn folder(&self, _id: &str) -> Option<PathBuf> {
        None
    }

    fn create_label(&self, _name: &str) -> Result<Label, LabelError> {
        Err(LabelError::Unavailable(self.0.clone()))
    }

    fn rename_label(&self, _label: &str, _name: &str) -> Result<(), LabelError> {
        Err(LabelError::Unavailable(self.0.clone()))
    }

    fn delete_label(&self, _label: &str) -> Result<(), LabelError> {
        Err(LabelError::Unavailable(self.0.clone()))
    }

    fn set_label(&self, _label: &str, _item: &str, _on: bool) -> Result<(), LabelError> {
        Err(LabelError::Unavailable(self.0.clone()))
    }
}

/// Bring the item's program to the front when it already runs, otherwise open it and
/// bring its window to the front once it appears.
pub fn start(
    library: &dyn GameLibrary,
    launcher: &dyn Launcher,
    id: &str,
) -> Result<(), PressError> {
    let start = library.start(id).ok_or(PressError::NotFound)?;
    let program = start.folder.map(Program::Folder);
    if let Some(program) = &program
        && launcher.focus(program).map_err(PressError::Launch)?
    {
        return Ok(());
    }
    launcher
        .open(&start.open, None, false)
        .map_err(PressError::Launch)?;
    if let Some(program) = program {
        launcher.focus_when_ready(program);
    }
    Ok(())
}

/// Button ID -> its pins, in order.
pub type Pinned = HashMap<String, Vec<Pin>>;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pin {
    pub id: String,
    pub name: String,
}

/// Pinned items per button, in pinning order, persisted in `SQLite`.
pub struct Pins {
    conn: Mutex<Connection>,
}

impl Pins {
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
            "CREATE TABLE IF NOT EXISTS library_pins (
                 button_id TEXT NOT NULL,
                 item_id   TEXT NOT NULL,
                 name      TEXT NOT NULL,
                 position  INTEGER NOT NULL,
                 PRIMARY KEY (button_id, item_id)
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Button ID -> its pins.
    pub fn all(&self) -> rusqlite::Result<Pinned> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT button_id, item_id, name FROM library_pins ORDER BY position")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Pin {
                    id: row.get(1)?,
                    name: row.get(2)?,
                },
            ))
        })?;
        let mut all = Pinned::new();
        for row in rows {
            let (button, pin) = row?;
            all.entry(button).or_default().push(pin);
        }
        Ok(all)
    }

    /// Pin at the end, or refresh the name of an existing pin in place.
    pub fn add(&self, button: &str, pin: &Pin) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO library_pins (button_id, item_id, name, position)
             VALUES (?1, ?2, ?3,
                     (SELECT COALESCE(MAX(position), 0) + 1 FROM library_pins))
             ON CONFLICT (button_id, item_id) DO UPDATE SET name = excluded.name",
            params![button, pin.id, pin.name],
        )?;
        Ok(())
    }

    pub fn remove(&self, button: &str, id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM library_pins WHERE button_id = ?1 AND item_id = ?2",
            params![button, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
pub mod fake {
    use std::{collections::BTreeSet, path::PathBuf, sync::Mutex};

    use super::{GameLibrary, Item, Label, LabelError, Listing, Picture, Start};

    /// Items `"1"` (installed in `C:\Games\One`, a picture on the web) and `"2"` (not
    /// installed, a picture file); labels `hidden` (fixed, named 非表示, holding "2") and
    /// `uc-1` (outdate, empty), kept in memory.
    pub struct FakeLibrary {
        labels: Mutex<Vec<(Label, BTreeSet<String>)>>,
    }

    impl Default for FakeLibrary {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FakeLibrary {
        pub fn new() -> Self {
            let label = |id: &str, name: &str, editable| Label {
                id: id.into(),
                name: name.into(),
                editable,
            };
            Self {
                labels: Mutex::new(vec![
                    (label("hidden", "非表示", false), ["2".to_owned()].into()),
                    (label("uc-1", "outdate", true), BTreeSet::new()),
                ]),
            }
        }
    }

    impl GameLibrary for FakeLibrary {
        fn listing(&self) -> Listing {
            let labels = self.labels.lock().unwrap();
            let item = |id: &str, name: &str, installed| Item {
                id: id.into(),
                name: name.into(),
                installed,
                labels: labels
                    .iter()
                    .filter(|(_, items)| items.contains(id))
                    .map(|(label, _)| label.id.clone())
                    .collect(),
            };
            Listing {
                items: vec![item("1", "One", true), item("2", "Two", false)],
                labels: labels.iter().map(|(label, _)| label.clone()).collect(),
                partial: None,
                labels_locked: None,
            }
        }

        fn folder(&self, id: &str) -> Option<PathBuf> {
            (id == "1").then(|| PathBuf::from(r"C:\Games\One"))
        }

        fn create_label(&self, name: &str) -> Result<Label, LabelError> {
            let mut labels = self.labels.lock().unwrap();
            let label = Label {
                id: format!("uc-{}", labels.len() + 1),
                name: name.into(),
                editable: true,
            };
            labels.push((label.clone(), BTreeSet::new()));
            Ok(label)
        }

        fn rename_label(&self, label: &str, name: &str) -> Result<(), LabelError> {
            let mut labels = self.labels.lock().unwrap();
            let found = labels
                .iter_mut()
                .find(|(l, _)| l.id == label)
                .ok_or(LabelError::NotFound)?;
            found.0.name = name.into();
            Ok(())
        }

        fn delete_label(&self, label: &str) -> Result<(), LabelError> {
            let mut labels = self.labels.lock().unwrap();
            let before = labels.len();
            labels.retain(|(l, _)| l.id != label);
            if labels.len() == before {
                return Err(LabelError::NotFound);
            }
            Ok(())
        }

        fn set_label(&self, label: &str, item: &str, on: bool) -> Result<(), LabelError> {
            let mut labels = self.labels.lock().unwrap();
            let (_, items) = labels
                .iter_mut()
                .find(|(l, _)| l.id == label)
                .ok_or(LabelError::NotFound)?;
            if on {
                items.insert(item.to_owned());
            } else {
                items.remove(item);
            }
            Ok(())
        }

        fn start(&self, id: &str) -> Option<Start> {
            match id {
                "1" => Some(Start {
                    open: "game://run/1".into(),
                    folder: Some(PathBuf::from(r"C:\Games\One")),
                }),
                "2" => Some(Start {
                    open: "game://install/2".into(),
                    folder: None,
                }),
                _ => None,
            }
        }

        fn picture(&self, id: &str) -> Option<Picture> {
            match id {
                "1" => Some(Picture::Url("https://img/1.jpg".into())),
                "2" => Some(Picture::File(PathBuf::from("Cargo.toml"))),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Label, LabelError, Pin, Pins, check_name, fake::FakeLibrary, start};
    use crate::{
        buttons::PressError,
        launch::{Program, fake::FakeLauncher},
    };

    fn pin(id: &str, name: &str) -> Pin {
        Pin {
            id: id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn pins_keep_their_order_per_button() {
        let pins = Pins::in_memory().unwrap();
        pins.add("steam", &pin("1", "One")).unwrap();
        pins.add("steam", &pin("2", "Two")).unwrap();
        pins.add("dlsite", &pin("9", "Nine")).unwrap();
        pins.add("steam", &pin("1", "One renamed")).unwrap();
        let all = pins.all().unwrap();
        assert_eq!(all["steam"], [pin("1", "One renamed"), pin("2", "Two")]);
        assert_eq!(all["dlsite"], [pin("9", "Nine")]);

        pins.remove("steam", "1").unwrap();
        pins.remove("steam", "missing").unwrap();
        pins.add("steam", &pin("1", "One")).unwrap();
        assert_eq!(
            pins.all().unwrap()["steam"],
            [pin("2", "Two"), pin("1", "One")]
        );
    }

    #[test]
    fn pins_survive_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("windows-link-pins-{}", std::process::id()));
        let path = dir.join("windows-link.db");
        Pins::open(&path)
            .unwrap()
            .add("steam", &pin("1", "One"))
            .unwrap();
        assert_eq!(
            Pins::open(&path).unwrap().all().unwrap()["steam"],
            [pin("1", "One")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_label_name_is_trimmed_and_must_be_new() {
        let labels = [Label {
            id: "uc-1".into(),
            name: "Outdate".into(),
            editable: true,
        }];
        assert_eq!(check_name("  RPG ", &labels, None), Ok("RPG".to_owned()));
        assert!(matches!(
            check_name(" ", &labels, None),
            Err(LabelError::Invalid(_))
        ));
        assert!(matches!(
            check_name("outdate", &labels, None),
            Err(LabelError::Invalid(_))
        ));
        // Renaming a label to a different case of its own name is fine.
        assert_eq!(
            check_name("outdate", &labels, Some("uc-1")),
            Ok("outdate".to_owned())
        );
    }

    #[test]
    fn starting_an_installed_game_waits_to_bring_it_forward() {
        let launcher = FakeLauncher::default();
        start(&FakeLibrary::new(), &launcher, "1").unwrap();
        assert_eq!(*launcher.opened.lock().unwrap(), ["game://run/1"]);
        assert_eq!(
            *launcher.awaited.lock().unwrap(),
            [Program::Folder(PathBuf::from(r"C:\Games\One"))]
        );
    }

    #[test]
    fn a_running_game_comes_to_the_front_instead_of_starting_again() {
        let launcher = FakeLauncher::default();
        launcher
            .running_folders
            .lock()
            .unwrap()
            .push(PathBuf::from(r"C:\Games\One"));
        start(&FakeLibrary::new(), &launcher, "1").unwrap();
        assert!(launcher.opened.lock().unwrap().is_empty());
        assert_eq!(
            *launcher.focused.lock().unwrap(),
            [Program::Folder(PathBuf::from(r"C:\Games\One"))]
        );
    }

    #[test]
    fn a_game_that_is_not_installed_opens_its_install_and_unknown_ones_are_not_found() {
        let launcher = FakeLauncher::default();
        start(&FakeLibrary::new(), &launcher, "2").unwrap();
        assert_eq!(*launcher.opened.lock().unwrap(), ["game://install/2"]);
        assert!(launcher.awaited.lock().unwrap().is_empty());
        assert_eq!(
            start(&FakeLibrary::new(), &launcher, "3"),
            Err(PressError::NotFound)
        );
    }
}
