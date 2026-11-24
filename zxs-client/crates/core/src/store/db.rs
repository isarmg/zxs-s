use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::domain::{LocalPath, PendingCommit, Result, Track};

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS tracks(key TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS pending(key TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;";

pub struct Db(Connection);

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self(connection))
    }

    pub fn track(&self, path: &LocalPath) -> Result<Option<Track>> {
        self.one("SELECT data FROM tracks WHERE key=?1", path)
    }

    pub fn pending(&self, path: &LocalPath) -> Result<Option<PendingCommit>> {
        self.one("SELECT data FROM pending WHERE key=?1", path)
    }

    fn one<T: DeserializeOwned>(&self, sql: &str, path: &LocalPath) -> Result<Option<T>> {
        let json: Option<String> = self
            .0
            .query_row(sql, [key(path)], |r| r.get(0))
            .optional()?;
        json.as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(Into::into)
    }

    pub fn all_tracks(&self) -> Result<Vec<Track>> {
        self.all("SELECT data FROM tracks ORDER BY key")
    }
    pub fn all_pending(&self) -> Result<Vec<PendingCommit>> {
        self.all("SELECT data FROM pending ORDER BY key")
    }

    fn all<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let mut statement = self.0.prepare_cached(sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn start(&self, pending: &PendingCommit) -> Result<()> {
        self.0.execute(
            "INSERT INTO pending VALUES(?1,?2)",
            params![key(&pending.path), json(pending)?],
        )?;
        Ok(())
    }

    pub fn abandon(&self, path: &LocalPath) -> Result<()> {
        self.0
            .execute("DELETE FROM pending WHERE key=?1", [key(path)])?;
        Ok(())
    }

    pub fn save(&self, track: &Track) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        let key = key(&track.path);
        tx.execute(
            "INSERT OR REPLACE INTO tracks VALUES(?1,?2)",
            params![key, json(track)?],
        )?;
        tx.execute("DELETE FROM pending WHERE key=?1", [key])?;
        tx.commit()?;
        Ok(())
    }
}

fn key(path: &LocalPath) -> String {
    path.as_str().to_lowercase()
}
fn json(data: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(data)?)
}
