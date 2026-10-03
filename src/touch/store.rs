//! Per-monitor "keep cursor" settings persisted in `SQLite`.

use std::{collections::HashMap, path::Path, sync::Mutex};

use rusqlite::{Connection, params};

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
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
            "CREATE TABLE IF NOT EXISTS touch_monitors (
                 monitor_id  TEXT PRIMARY KEY,
                 keep_cursor INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn keep_cursor_overrides(&self) -> rusqlite::Result<HashMap<String, bool>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare("SELECT monitor_id, keep_cursor FROM touch_monitors")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)))?;
        rows.collect()
    }

    pub fn set_keep_cursor(&self, monitor_id: &str, keep: bool) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO touch_monitors (monitor_id, keep_cursor) VALUES (?1, ?2)
             ON CONFLICT(monitor_id) DO UPDATE SET keep_cursor = excluded.keep_cursor",
            params![monitor_id, i64::from(keep)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn stores_and_overwrites_settings() {
        let store = Store::in_memory().unwrap();
        assert!(store.keep_cursor_overrides().unwrap().is_empty());
        store.set_keep_cursor("mon-a", false).unwrap();
        store.set_keep_cursor("mon-b", true).unwrap();
        store.set_keep_cursor("mon-a", true).unwrap();
        let map = store.keep_cursor_overrides().unwrap();
        assert_eq!(map.len(), 2);
        assert!(map["mon-a"]);
        assert!(map["mon-b"]);
    }

    #[test]
    fn settings_survive_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("windows-link-test-{}", std::process::id()));
        let path = dir.join("settings.db");
        {
            let store = Store::open(&path).unwrap();
            store.set_keep_cursor("mon-x", false).unwrap();
        }
        let reopened = Store::open(&path).unwrap();
        assert!(!reopened.keep_cursor_overrides().unwrap()["mon-x"]);
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }
}
