//! Which `automation.hold` buttons are on. While one is, the user is using the PC
//! and automation (the mouse wheel API) must wait. Kept in `windows-link.db` so a
//! restart, such as an update, does not let automation in while the user plays.

use std::{collections::HashSet, path::Path, sync::Mutex};

use rusqlite::Connection;

pub struct Holds {
    conn: Mutex<Connection>,
}

impl Holds {
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
            "CREATE TABLE IF NOT EXISTS automation_holds (button_id TEXT PRIMARY KEY);",
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

    /// The buttons that are on.
    pub fn held(&self) -> rusqlite::Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT button_id FROM automation_holds")?;
        stmt.query_map([], |row| row.get(0))?.collect()
    }

    pub fn set(&self, button: &str, on: bool) -> rusqlite::Result<()> {
        let conn = self.conn();
        if on {
            conn.execute(
                "INSERT OR IGNORE INTO automation_holds (button_id) VALUES (?1)",
                [button],
            )?;
        } else {
            conn.execute(
                "DELETE FROM automation_holds WHERE button_id = ?1",
                [button],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Holds;

    #[test]
    fn a_hold_stays_on_after_reopening() {
        let path =
            std::env::temp_dir().join(format!("windows-link-holds-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let holds = Holds::open(&path).unwrap();
            holds.set("hands", true).unwrap();
            holds.set("hands", true).unwrap();
        }
        let holds = Holds::open(&path).unwrap();
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
