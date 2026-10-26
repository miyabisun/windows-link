//! The resting state in `windows-link.db`, so a restart (an update) keeps it: the
//! monitor kept lit while resting, and each darkened monitor's own brightness until it
//! is put back.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, PoisonError},
};

use rusqlite::{Connection, OptionalExtension, params};

pub struct RestStore {
    conn: Mutex<Connection>,
}

impl RestStore {
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
            "CREATE TABLE IF NOT EXISTS rest_keep (monitor_id TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS rest_brightness (
                 monitor_id TEXT PRIMARY KEY,
                 brightness INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The monitor kept lit, while resting.
    pub fn keep(&self) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row("SELECT monitor_id FROM rest_keep", [], |row| row.get(0))
            .optional()
    }

    /// Each darkened monitor's own brightness, by monitor id.
    pub fn brightness(&self) -> rusqlite::Result<HashMap<String, u32>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT monitor_id, brightness FROM rest_brightness")?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }

    /// Start resting with `keep` lit, saving `saved` brightness; a monitor already saved
    /// keeps its value.
    pub fn start(&self, keep: &str, saved: &[(String, u32)]) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM rest_keep", [])?;
        tx.execute("INSERT INTO rest_keep (monitor_id) VALUES (?1)", [keep])?;
        for (monitor, brightness) in saved {
            tx.execute(
                "INSERT OR IGNORE INTO rest_brightness (monitor_id, brightness) VALUES (?1, ?2)",
                params![monitor, brightness],
            )?;
        }
        tx.commit()
    }

    /// The monitor's brightness is back.
    pub fn forget(&self, monitor_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM rest_brightness WHERE monitor_id = ?1",
            [monitor_id],
        )?;
        Ok(())
    }

    /// No longer resting; brightness not yet put back stays saved.
    pub fn stop(&self) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM rest_keep", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::RestStore;

    #[test]
    fn keeps_the_rest_and_never_overwrites_a_saved_brightness() {
        let store = RestStore::in_memory().unwrap();
        assert_eq!(store.keep().unwrap(), None);
        store.start("panel", &[("main".into(), 40)]).unwrap();
        assert_eq!(store.keep().unwrap().as_deref(), Some("panel"));
        store
            .start("panel", &[("main".into(), 0), ("side".into(), 70)])
            .unwrap();
        assert_eq!(
            store.brightness().unwrap(),
            HashMap::from([("main".into(), 40), ("side".into(), 70)])
        );
        store.forget("main").unwrap();
        store.stop().unwrap();
        assert_eq!(store.keep().unwrap(), None);
        assert_eq!(
            store.brightness().unwrap(),
            HashMap::from([("side".into(), 70)])
        );
    }

    #[test]
    fn the_rest_survives_reopening_the_file() {
        let dir =
            std::env::temp_dir().join(format!("windows-link-rest-test-{}", std::process::id()));
        let path = dir.join("windows-link.db");
        {
            let store = RestStore::open(&path).unwrap();
            store.start("panel", &[("main".into(), 40)]).unwrap();
        }
        let reopened = RestStore::open(&path).unwrap();
        assert_eq!(reopened.keep().unwrap().as_deref(), Some("panel"));
        assert_eq!(reopened.brightness().unwrap()["main"], 40);
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }
}
