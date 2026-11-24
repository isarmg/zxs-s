mod rows;
use std::path::Path;
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicUsize, Ordering},
};

use rows::{json, many, one, summarize};
use rusqlite::{Connection, Transaction, params};

use crate::domain::{
    Blake3Hash, Change, Container, ContainerId, ContainerPage, Detail, Error, FileName, Filter,
    FolderPath, FolderSummary, OpId, Record, RecycleFilter, RecycleId, RecyclePage, Recycled,
    Result, SessionEffect, Version, VersionNum,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS containers(
    id TEXT PRIMARY KEY, folder TEXT NOT NULL, name_key TEXT NOT NULL, data TEXT NOT NULL,
    UNIQUE(folder,name_key)
) STRICT;
CREATE TABLE IF NOT EXISTS versions(
    id TEXT NOT NULL REFERENCES containers(id) ON DELETE CASCADE,
    version INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(id,version)
) STRICT;
CREATE TABLE IF NOT EXISTS tags(
    id TEXT NOT NULL REFERENCES containers(id) ON DELETE CASCADE,
    tag TEXT NOT NULL, PRIMARY KEY(id,tag)
) STRICT;
CREATE INDEX IF NOT EXISTS by_tag ON tags(tag,id);
CREATE TABLE IF NOT EXISTS recycle_bin(id TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS sessions(
    token_blake3 TEXT PRIMARY KEY, expires_at INTEGER NOT NULL, generation TEXT NOT NULL
) STRICT;";

pub struct Db {
    writer: Mutex<Connection>,
    readers: [Mutex<Connection>; 8],
    cursor: AtomicUsize,
}

impl Db {
    pub fn open(path: &Path) -> Result<(Self, bool)> {
        let writer = Connection::open(path)?;
        writer.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let missing = writer.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='containers')",
            [],
            |r| r.get(0),
        )?;
        let readers = (0..8)
            .map(|_| {
                let connection = Connection::open(path)?;
                connection.execute_batch("PRAGMA query_only=ON;")?;
                Ok(Mutex::new(connection))
            })
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .map_err(|_| Error::Recovery)?;
        Ok((
            Self {
                writer: Mutex::new(writer),
                readers,
                cursor: AtomicUsize::new(0),
            },
            missing,
        ))
    }

    pub fn rebuild(&self, build: impl FnOnce(&Transaction<'_>) -> Result<()>) -> Result<()> {
        self.write(|tx| {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch("DELETE FROM containers; DELETE FROM recycle_bin; DELETE FROM operations; DELETE FROM sessions;")?;
            build(tx)
        })
    }

    pub fn write(&self, change: impl FnOnce(&Transaction<'_>) -> Result<()>) -> Result<()> {
        let writer = self.writer.lock().map_err(|_| Error::Recovery)?;
        let tx = writer.unchecked_transaction()?;
        change(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn read(&self) -> Result<MutexGuard<'_, Connection>> {
        let i = self.cursor.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[i].lock().map_err(|_| Error::Recovery)
    }

    pub fn container(&self, id: &ContainerId) -> Result<Option<Container>> {
        one(
            &*self.read()?,
            "SELECT data FROM containers WHERE id=?1",
            [id.as_str()],
        )
    }
    pub fn version(&self, id: &ContainerId, version: VersionNum) -> Result<Option<Version>> {
        one(
            &*self.read()?,
            "SELECT data FROM versions WHERE id=?1 AND version=?2",
            params![id.as_str(), version.get()],
        )
    }
    pub fn detail(&self, id: &ContainerId) -> Result<Option<Detail>> {
        let conn = self.read()?;
        let tx = conn.unchecked_transaction()?;
        let Some(container) = one(
            &tx,
            "SELECT data FROM containers WHERE id=?1",
            [id.as_str()],
        )?
        else {
            return Ok(None);
        };
        let versions = many(
            &tx,
            "SELECT data FROM versions WHERE id=?1 ORDER BY version",
            [id.as_str()],
        )?;
        Ok(Some(Detail {
            container,
            versions,
        }))
    }
    pub fn recycled(&self, id: &RecycleId) -> Result<Option<Recycled>> {
        one(
            &*self.read()?,
            "SELECT data FROM recycle_bin WHERE id=?1",
            [id.as_str()],
        )
    }
    pub fn record(&self, op: &OpId) -> Result<Option<Record>> {
        one(
            &*self.read()?,
            "SELECT data FROM operations WHERE id=?1",
            [op.as_str()],
        )
    }

    pub fn name_taken(
        &self,
        folder: &FolderPath,
        name: &FileName,
        except: Option<&ContainerId>,
    ) -> Result<bool> {
        Ok(self.read()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM containers WHERE folder=?1 AND name_key=?2 AND (?3 IS NULL OR id!=?3))",
            params![folder.as_str(), name.key(), except.map(ContainerId::as_str)], |r| r.get(0)
        )?)
    }

    pub fn session(
        &self,
        token_hash: &Blake3Hash,
        generation: &Blake3Hash,
        at: u64,
    ) -> Result<bool> {
        Ok(self.read()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE token_blake3=?1 AND generation=?2 AND expires_at>?3)",
            params![token_hash.as_str(), generation.as_str(), at], |r| r.get(0)
        )?)
    }

    pub fn containers(&self, f: Filter) -> Result<ContainerPage> {
        let conn = self.read()?;
        let tx = conn.unchecked_transaction()?;
        let folder = f.folder.as_ref().map(FolderPath::as_str);
        let tag = f.tag.as_deref();
        let q = f.q.map(|q| q.to_lowercase());
        let cursor = f.cursor.as_ref().map_or("", ContainerId::as_str);
        let limit = page_limit(f.limit);
        let mut containers: Vec<Container> = many(
            &tx,
            "
            SELECT data FROM containers AS c WHERE id>?1 AND (?2 IS NULL OR folder=?2)
              AND (?3 IS NULL OR EXISTS(SELECT 1 FROM tags WHERE id=c.id AND tag=?3))
              AND (?4 IS NULL OR instr(name_key,?4)>0)
            ORDER BY id LIMIT ?5",
            params![cursor, folder, tag, q, limit + 1],
        )?;
        let more = containers.len() > limit as usize;
        containers.truncate(limit as usize);
        let next_cursor = containers.last().filter(|_| more).map(|c| c.id.clone());
        let folders = match f.folder {
            Some(folder) => folder_summaries(&tx, &folder, tag, q.as_deref())?,
            None => Vec::new(),
        };
        Ok(ContainerPage {
            containers,
            folders,
            next_cursor,
        })
    }

    pub fn recycle(&self, f: &RecycleFilter) -> Result<RecyclePage> {
        let limit = page_limit(f.limit);
        let cursor = f.cursor.as_ref().map_or("", RecycleId::as_str);
        let mut entries: Vec<Recycled> = many(
            &*self.read()?,
            "SELECT data FROM recycle_bin WHERE id>?1 ORDER BY id LIMIT ?2",
            params![cursor, limit + 1],
        )?;
        let more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let next_cursor = entries
            .last()
            .filter(|_| more)
            .map(|e| e.recycle.recycle_id.clone());
        Ok(RecyclePage {
            entries,
            next_cursor,
        })
    }
}

fn folder_summaries(
    conn: &Connection,
    folder: &FolderPath,
    tag: Option<&str>,
    q: Option<&str>,
) -> Result<Vec<FolderSummary>> {
    let mut statement = conn.prepare_cached(
        "
        SELECT folder,count(*) FROM containers AS c WHERE folder!=?1 AND instr(folder,?2)=1
          AND (?3 IS NULL OR EXISTS(SELECT 1 FROM tags WHERE id=c.id AND tag=?3))
          AND (?4 IS NULL OR instr(name_key,?4)>0) GROUP BY folder",
    )?;
    let prefix = format!("{}/", folder.as_str().trim_end_matches('/'));
    summarize(
        folder,
        &prefix,
        statement.query(params![folder.as_str(), prefix, tag, q])?,
    )
}

fn page_limit(limit: Option<u16>) -> u16 {
    limit.unwrap_or(100).clamp(1, 200)
}

pub fn put_container(tx: &Transaction<'_>, c: &Container) -> Result<()> {
    let id = c.id.as_str();
    tx.execute("INSERT INTO containers VALUES(?1,?2,?3,?4)
        ON CONFLICT(id) DO UPDATE SET folder=excluded.folder,name_key=excluded.name_key,data=excluded.data",
        params![id, c.folder.as_str(), c.name.key(), json(c)?])?;
    tx.execute("DELETE FROM tags WHERE id=?1", [id])?;
    let mut insert = tx.prepare_cached("INSERT INTO tags VALUES(?1,?2)")?;
    for tag in &c.tags {
        insert.execute([id, tag])?;
    }
    Ok(())
}

pub fn put_detail(tx: &Transaction<'_>, detail: &Detail) -> Result<()> {
    put_container(tx, &detail.container)?;
    for v in &detail.versions {
        put_version(tx, &detail.container.id, v)?;
    }
    Ok(())
}

fn put_version(tx: &Transaction<'_>, id: &ContainerId, version: &Version) -> Result<()> {
    tx.execute("INSERT INTO versions VALUES(?1,?2,?3) ON CONFLICT(id,version) DO UPDATE SET data=excluded.data",
        params![id.as_str(), version.number.get(), json(version)?])?;
    Ok(())
}

pub fn put_recycle(tx: &Transaction<'_>, entry: &Recycled) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO recycle_bin VALUES(?1,?2)",
        params![entry.recycle.recycle_id.as_str(), json(entry)?],
    )?;
    Ok(())
}

pub fn put_record(tx: &Transaction<'_>, record: &Record) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO operations VALUES(?1,?2)",
        params![record.receipt.op_id.as_str(), json(record)?],
    )?;
    Ok(())
}

pub fn put_session_effect(
    tx: &Transaction<'_>,
    effect: &SessionEffect,
    generation: &Blake3Hash,
    at: u64,
) -> Result<()> {
    match effect {
        SessionEffect::Login {
            token_blake3,
            expires_at,
            generation: login_generation,
        } if login_generation == generation && *expires_at > at => {
            tx.execute("DELETE FROM sessions WHERE expires_at<=?1", [at])?;
            tx.execute(
                "INSERT OR REPLACE INTO sessions VALUES(?1,?2,?3)",
                params![token_blake3.as_str(), expires_at, generation.as_str()],
            )?;
        }
        SessionEffect::Logout { token_blake3 } => {
            tx.execute(
                "DELETE FROM sessions WHERE token_blake3=?1",
                [token_blake3.as_str()],
            )?;
        }
        SessionEffect::Login { .. } => {}
    }
    Ok(())
}

pub fn apply_change(
    tx: &Transaction<'_>,
    change: &Change,
    restored: Option<&Detail>,
) -> Result<()> {
    match change {
        Change::Publish { commit } => {
            put_container(tx, &commit.container)?;
            put_version(tx, &commit.container.id, &commit.version)
        }
        Change::Patch { container } => put_container(tx, container),
        Change::Recycle { entry } => {
            tx.execute(
                "DELETE FROM containers WHERE id=?1",
                [entry.container.id.as_str()],
            )?;
            put_recycle(tx, entry)
        }
        Change::Restore { recycle_id, .. } => {
            tx.execute("DELETE FROM recycle_bin WHERE id=?1", [recycle_id.as_str()])?;
            put_detail(tx, restored.ok_or(Error::Recovery)?)
        }
        Change::Prune {
            container_id,
            versions,
        } => {
            let mut delete =
                tx.prepare_cached("DELETE FROM versions WHERE id=?1 AND version=?2")?;
            for v in versions {
                delete.execute(params![container_id.as_str(), v.get()])?;
            }
            Ok(())
        }
    }
}
