//! Which toggle buttons are on, kept in `windows-link.db` so a restart, such as an
//! update, keeps them: the `automation.hold` buttons (while one is, the user is using
//! the PC and automation, the mouse wheel API, must wait) and the `cursor.highlight`
//! buttons, each kind in a table of its own; and the colour chosen for each
//! `cursor.highlight` button.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Mutex,
};

use rusqlite::Connection;

use crate::cursor::Palette;

/// The `automation.hold` buttons' table.
pub const HOLDS: &str = "automation_holds";
/// The `cursor.highlight` buttons' table.
pub const HIGHLIGHTS: &str = "cursor_highlights";

pub struct Holds {
    conn: Mutex<Connection>,
    table: &'static str,
}

impl Holds {
    /// The buttons kept in `table` (`HOLDS` or `HIGHLIGHTS`).
    pub fn open(path: &Path, table: &'static str) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?, table)
    }

    pub fn in_memory(table: &'static str) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, table)
    }

    fn init(conn: Connection, table: &'static str) -> rusqlite::Result<Self> {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {table} (button_id TEXT PRIMARY KEY);"
        ))?;
        Ok(Self {
            conn: Mutex::new(conn),
            table,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The buttons that are on.
    pub fn held(&self) -> rusqlite::Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT button_id FROM {}", self.table))?;
        stmt.query_map([], |row| row.get(0))?.collect()
    }

    pub fn set(&self, button: &str, on: bool) -> rusqlite::Result<()> {
        let conn = self.conn();
        if on {
            conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO {} (button_id) VALUES (?1)",
                    self.table
                ),
                [button],
            )?;
        } else {
            conn.execute(
                &format!("DELETE FROM {} WHERE button_id = ?1", self.table),
                [button],
            )?;
        }
        Ok(())
    }
}

/// The colour chosen on the panel for each `cursor.highlight` button (blue, the
/// default, for one never chosen).
pub struct Colors {
    conn: Mutex<Connection>,
}

impl Colors {
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
            "CREATE TABLE IF NOT EXISTS cursor_colors \
             (button_id TEXT PRIMARY KEY, color TEXT NOT NULL);",
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

    /// The colours chosen, by button; a colour this version does not know is left out.
    pub fn all(&self) -> rusqlite::Result<HashMap<String, Palette>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT button_id, color FROM cursor_colors")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut colors = HashMap::new();
        for row in rows {
            let (button, name) = row?;
            if let Some(palette) = Palette::named(&name) {
                colors.insert(button, palette);
            }
        }
        Ok(colors)
    }

    pub fn set(&self, button: &str, palette: Palette) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO cursor_colors (button_id, color) VALUES (?1, ?2) \
             ON CONFLICT(button_id) DO UPDATE SET color = excluded.color",
            [button, palette.name()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Colors, HIGHLIGHTS, HOLDS, Holds};
    use crate::cursor::Palette;

    /// A highlight button's colour is blue until chosen, and stays chosen after
    /// reopening.
    #[test]
    fn a_chosen_colour_stays_after_reopening() {
        let path =
            std::env::temp_dir().join(format!("windows-link-colors-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let colors = Colors::open(&path).unwrap();
            assert!(colors.all().unwrap().is_empty());
            colors.set("cursor", Palette::Red).unwrap();
            colors.set("cursor", Palette::Green).unwrap();
        }
        let colors = Colors::open(&path).unwrap();
        assert_eq!(
            colors.all().unwrap().into_iter().collect::<Vec<_>>(),
            [("cursor".to_owned(), Palette::Green)]
        );
        drop(colors);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_hold_stays_on_after_reopening() {
        let path =
            std::env::temp_dir().join(format!("windows-link-holds-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let holds = Holds::open(&path, HOLDS).unwrap();
            holds.set("hands", true).unwrap();
            holds.set("hands", true).unwrap();
        }
        let holds = Holds::open(&path, HOLDS).unwrap();
        // Another kind's table in the same file keeps its buttons apart.
        let highlights = Holds::open(&path, HIGHLIGHTS).unwrap();
        assert!(highlights.held().unwrap().is_empty());
        assert_eq!(
            holds.held().unwrap().into_iter().collect::<Vec<_>>(),
            ["hands"]
        );
        holds.set("hands", false).unwrap();
        assert!(holds.held().unwrap().is_empty());
        drop(holds);
        let _ = std::fs::remove_file(&path);
    }
}
