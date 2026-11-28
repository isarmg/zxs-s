use std::{collections::BTreeSet, path::Path};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Params, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::domain::{ContainerId, InterruptedCheckout, LocalPath, Result, Track};

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS tracks(key TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS tracks_container ON tracks(json_extract(data, '$.container_id'));
CREATE TABLE IF NOT EXISTS interrupted_checkout(id TEXT PRIMARY KEY, key TEXT UNIQUE NOT NULL, data TEXT NOT NULL) STRICT;";

pub struct Db(Connection);

impl Db {
    pub fn read_only(path: &Path) -> Result<Option<Self>> {
        if !path.try_exists()? {
            return Ok(None);
        }
        Ok(Some(Self(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self(connection))
    }

    pub fn track(&self, path: &LocalPath) -> Result<Option<Track>> {
        self.one("SELECT data FROM tracks WHERE key=?1", [path.key()])
    }

    pub fn occupant(&self, path: &LocalPath, id: &ContainerId) -> Result<Option<ContainerId>> {
        self.one(
            "SELECT json_quote(json_extract(data,'$.container_id')) FROM tracks
             WHERE key=?1 AND json_extract(data,'$.container_id')<>?2",
            params![path.key(), id.as_str()],
        )
    }

    pub fn reserved_paths(&self) -> Result<BTreeSet<String>> {
        let keys = "SELECT json_quote(key) FROM tracks UNION SELECT json_quote(key) FROM interrupted_checkout";
        Ok(self.all(keys, [])?.into_iter().collect())
    }

    pub fn track_for_container(&self, id: &ContainerId) -> Result<Option<Track>> {
        self.one(
            "SELECT data FROM tracks WHERE json_extract(data, '$.container_id')=?1",
            [id.as_str()],
        )
    }

    pub fn bind_container(&self, id: &ContainerId, endpoint: &str) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "UPDATE tracks SET data=json_set(data,'$.endpoint',?2,'$.remote_missing',json('false'))
             WHERE json_extract(data,'$.container_id')=?1
             AND (json_extract(data,'$.endpoint')<>?2 OR json_extract(data,'$.remote_missing'))",
            params![id.as_str(), endpoint],
        )?;
        tx.execute(
            "UPDATE interrupted_checkout SET data=json_set(data,'$.endpoint',?2)
             WHERE id=?1 AND json_extract(data,'$.endpoint')<>?2",
            params![id.as_str(), endpoint],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn one<T: DeserializeOwned>(&self, sql: &str, params: impl Params) -> Result<Option<T>> {
        let json: Option<String> = self
            .0
            .prepare_cached(sql)?
            .query_row(params, |r| r.get(0))
            .optional()?;
        json.as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(Into::into)
    }

    pub fn all_tracks(&self) -> Result<Vec<Track>> {
        self.all("SELECT data FROM tracks ORDER BY key", [])
    }

    pub fn tracks_in(&self, directory: &LocalPath) -> Result<Vec<Track>> {
        self.in_directory(directory, "data")
    }

    pub fn track_paths_in(&self, directory: &LocalPath) -> Result<Vec<LocalPath>> {
        self.in_directory(directory, "json_quote(json_extract(data,'$.path'))")
    }

    fn in_directory<T: DeserializeOwned>(
        &self,
        directory: &LocalPath,
        column: &str,
    ) -> Result<Vec<T>> {
        let select = format!("SELECT {column} FROM tracks");
        if directory.is_root() {
            return self.all(&format!("{select} ORDER BY key"), []);
        }
        let key = directory.key();
        self.all(
            &format!("{select} WHERE key=?1 OR (key>=?2 AND key<?3)"),
            params![key, format!("{key}/"), format!("{key}0")],
        )
    }

    pub fn overlapping_path(
        &self,
        path: &LocalPath,
        except: Option<&ContainerId>,
    ) -> Result<Option<LocalPath>> {
        let excluded = except.map_or("", ContainerId::as_str);
        let key = path.key();
        let mut ancestor = Some(path.clone());
        while let Some(candidate) = ancestor {
            let found = self.one(
                "SELECT json_quote(json_extract(data,'$.path')) FROM (
                   SELECT data FROM tracks WHERE key=?1
                   UNION ALL
                   SELECT data FROM interrupted_checkout WHERE key=?1)
                 WHERE ?1<>?3 OR json_extract(data,'$.container_id')<>?2 LIMIT 1",
                params![candidate.key(), excluded, key],
            )?;
            if found.is_some() {
                return Ok(found);
            }
            ancestor = candidate.parent();
        }
        if path.is_root() {
            return self.one(
                "SELECT json_quote(json_extract(data,'$.path')) FROM (
                   SELECT data FROM tracks UNION ALL SELECT data FROM interrupted_checkout)
                 WHERE json_extract(data,'$.container_id')<>?1 LIMIT 1",
                [excluded],
            );
        }
        self.one(
            "SELECT json_quote(json_extract(data,'$.path')) FROM (
               SELECT data FROM tracks WHERE key>=?1 AND key<?2
               UNION ALL SELECT data FROM interrupted_checkout WHERE key>=?1 AND key<?2)
             WHERE json_extract(data,'$.container_id')<>?3 LIMIT 1",
            params![format!("{key}/"), format!("{key}0"), excluded],
        )
    }
    pub fn interrupted_checkout(&self, id: &ContainerId) -> Result<Option<InterruptedCheckout>> {
        self.one(
            "SELECT data FROM interrupted_checkout WHERE id=?1",
            [id.as_str()],
        )
    }

    pub fn all_checkouts(&self) -> Result<Vec<InterruptedCheckout>> {
        self.all("SELECT data FROM interrupted_checkout ORDER BY key", [])
    }

    pub fn start_checkout(&self, checkout: &InterruptedCheckout) -> Result<()> {
        self.0.execute(
            "INSERT INTO interrupted_checkout VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET key=excluded.key,data=excluded.data",
            params![checkout.container_id.as_str(), checkout.path.key(), json(checkout)?],
        )?;
        Ok(())
    }

    fn all<T: DeserializeOwned>(&self, sql: &str, parameters: impl Params) -> Result<Vec<T>> {
        let mut statement = self.0.prepare_cached(sql)?;
        let rows = statement.query_map(parameters, |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn save(&self, track: &Track) -> Result<()> {
        self.save_checkout(track, None)
    }

    pub fn save_checkout(&self, track: &Track, replaced: Option<&LocalPath>) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        let key = track.path.key();
        if let Some(previous) = replaced.filter(|previous| previous.key() != key) {
            tx.execute("DELETE FROM tracks WHERE key=?1", [previous.key()])?;
        }
        tx.execute(
            "INSERT INTO tracks VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
            params![key, json(track)?],
        )?;
        tx.execute(
            "DELETE FROM interrupted_checkout WHERE id=?1",
            [track.container_id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn relocate(&self, old: &LocalPath, track: &Track) -> Result<()> {
        self.0.execute(
            "UPDATE tracks SET key=?1,data=?2 WHERE key=?3",
            params![track.path.key(), json(track)?, old.key()],
        )?;
        Ok(())
    }

    pub fn forget(&self, path: &LocalPath) -> Result<bool> {
        let tx = self.0.unchecked_transaction()?;
        let key = path.key();
        let tracks = tx.execute("DELETE FROM tracks WHERE key=?1", [&key])?;
        let checkouts = tx.execute("DELETE FROM interrupted_checkout WHERE key=?1", [&key])?;
        tx.commit()?;
        Ok(tracks != 0 || checkouts != 0)
    }

    pub fn remote_missing(&self, path: &LocalPath, missing: bool) -> Result<()> {
        self.0.execute(
            "UPDATE tracks SET data=json_set(data,'$.remote_missing',json(?2)) WHERE key=?1",
            params![path.key(), if missing { "true" } else { "false" }],
        )?;
        Ok(())
    }
}

fn json(data: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(data)?)
}
