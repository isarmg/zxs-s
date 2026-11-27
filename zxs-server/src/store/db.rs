mod paths;
mod queries;
mod rows;
pub(super) use queries::check_indexed_paths;
#[cfg(test)]
mod tests;

use std::{
    path::Path,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use rusqlite::{Connection, Transaction, params};

use super::Layout;
use crate::domain::{
    Change, Container, ContainerId, Detail, Error, FolderPath, Pending, Recycled, Result, Version,
    invalid,
};
use rows::json;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders(path_key TEXT PRIMARY KEY, parent_key TEXT NOT NULL, path TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS folder_parent ON folders(parent_key,path_key);
CREATE TABLE IF NOT EXISTS containers(
    id TEXT PRIMARY KEY, folder TEXT NOT NULL, folder_key TEXT NOT NULL,
    name TEXT NOT NULL, name_key TEXT NOT NULL, path_key TEXT NOT NULL UNIQUE, latest_blake3 TEXT NOT NULL, data TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS by_folder_key ON containers(folder_key,name_key,id);
CREATE TABLE IF NOT EXISTS versions(
    id TEXT NOT NULL REFERENCES containers(id) ON DELETE CASCADE,
    version INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(id,version)
) STRICT;
CREATE TABLE IF NOT EXISTS tags(
    id TEXT NOT NULL REFERENCES containers(id) ON DELETE CASCADE,
    tag TEXT NOT NULL, PRIMARY KEY(id,tag)
) STRICT;
CREATE INDEX IF NOT EXISTS by_tag ON tags(tag,id);
CREATE TABLE IF NOT EXISTS recycle_bin(id TEXT PRIMARY KEY, recycled_at INTEGER NOT NULL, recycle_disk TEXT NOT NULL, data TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS recycle_time ON recycle_bin(recycled_at DESC,id DESC);";

const COUNT_RECYCLE: &str = "SELECT count(*) FROM recycle_bin";
const FIND_CONTAINER: &str = "SELECT folder,name,latest_blake3,data FROM containers WHERE id=?1";
const FIND_VERSION: &str = "SELECT data FROM versions WHERE id=?1 AND version=?2";
const FIND_VERSIONS: &str = "SELECT data FROM versions WHERE id=?1 ORDER BY version";
const INDEXED_VERSIONS: &str =
    "SELECT c.folder,c.name,v.version,c.path_key FROM versions v JOIN containers c ON c.id=v.id";
const INDEXED_ARCHIVES: &str = "SELECT id,recycled_at,recycle_disk FROM recycle_bin";
const FIND_RECYCLED: &str = "SELECT data FROM recycle_bin WHERE id=?1";
const LIST_RECYCLED: &str = "SELECT data FROM recycle_bin WHERE (recycled_at,id)<(?1,?2) ORDER BY recycled_at DESC,id DESC LIMIT ?3";
const FIND_FOLDER: &str = "SELECT path FROM folders WHERE path_key=?1";
const FIND_PATH: &str = "SELECT id FROM containers WHERE path_key=?1";
// The cursor supplies the lower index bound, and the separate upper bound ends the scan.
const LIST_CONTAINERS: &str = "
    SELECT folder,name,latest_blake3,data FROM containers AS c WHERE (folder_key,name_key,id)>(?1,?2,?3)
      AND folder_key<=?9 AND (folder_key=?4 OR (folder_key>=?8 AND folder_key<?9))
      AND (?5 IS NULL OR EXISTS(SELECT 1 FROM tags WHERE id=c.id AND tag=?5))
      AND (?6 IS NULL OR instr(name_key,?6)>0) ORDER BY folder_key,name_key,id LIMIT ?7";
const LIST_FOLDERS: &str = "SELECT path FROM folders WHERE parent_key=?1 ORDER BY path_key";
const LIST_DESCENDANTS: &str =
    "SELECT path FROM folders WHERE path_key>?1 AND path_key>=?2 AND path_key<?3 ORDER BY path_key";
const LOOKUP_CONTAINERS: &str = "
    SELECT folder,name,latest_blake3,data FROM containers
    WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY folder_key,name_key,id";
const SUBTREE_CONTAINERS: &str = "SELECT folder,name,latest_blake3,data FROM containers WHERE folder_key=?1 OR (folder_key>=?2 AND folder_key<?3) ORDER BY folder_key,name_key,id";
const FOLDER_OCCUPIED: &str = "SELECT EXISTS(SELECT 1 FROM containers WHERE folder_key=?1)
    OR EXISTS(SELECT 1 FROM folders WHERE path_key>=?2 AND path_key<?3)";
const LONGEST_PATH: &str = "SELECT max(n) FROM (
    SELECT max(length(CAST(path AS BLOB))) AS n FROM folders WHERE path_key=?1 OR (path_key>=?2 AND path_key<?3)
    UNION ALL SELECT max(length(CAST(folder||'/'||name AS BLOB))) FROM containers
        WHERE folder_key=?1 OR (folder_key>=?2 AND folder_key<?3))";
const MOVE_FOLDERS: &str = "UPDATE folders SET
    path=?3||substr(path,length(?2)+1),path_key=?4||substr(path_key,length(?1)+1),
    parent_key=CASE WHEN path_key=?1 THEN ?7 ELSE ?4||substr(parent_key,length(?1)+1) END
    WHERE path_key=?1 OR (path_key>=?5 AND path_key<?6)";
const MOVE_CONTAINERS: &str = "UPDATE containers SET
    folder=?3||substr(folder,length(?2)+1),folder_key=?4||substr(folder_key,length(?1)+1),
    path_key=?4||substr(path_key,length(?1)+1)
    WHERE folder_key=?1 OR (folder_key>=?5 AND folder_key<?6)";

pub struct Db {
    writer: Mutex<Connection>,
    readers: [Mutex<Connection>; 8],
    cursor: AtomicUsize,
}

impl Db {
    pub fn open(path: &Path) -> Result<(Self, bool)> {
        let writer = rows::open(path)?;
        writer.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let empty = writer.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='containers')",
            [],
            |r| r.get(0),
        )?;
        let readers = (0..8)
            .map(|_| {
                let connection = rows::open(path)?;
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
            empty,
        ))
    }

    pub fn rebuild(
        &self,
        compare: Option<&Layout>,
        build: impl FnOnce(&Transaction<'_>) -> Result<()>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(
                "DELETE FROM folders; DELETE FROM containers; DELETE FROM recycle_bin;",
            )?;
            build(tx)?;
            if let Some(paths) = compare {
                // The WAL reader still sees the previous committed index.
                queries::compare_indexed_paths(paths, &*self.read()?, Some(tx))?;
            }
            Ok(())
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
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        rows::available_reader(&self.readers, start)
    }
}

pub fn put_folder(tx: &Transaction<'_>, folder: &FolderPath) -> Result<()> {
    let changes = tx.prepare_cached(
        "INSERT INTO folders VALUES(?1,?2,?3) ON CONFLICT(path_key) DO UPDATE SET path=excluded.path WHERE path=excluded.path",
    )?.execute(params![folder.key(), folder.parent().as_ref().map_or_else(String::new, FolderPath::key), folder.as_str()])?;
    if changes == 0 {
        return Err(invalid(format!("conflicting directory spelling: {folder}")));
    }
    Ok(())
}

fn put_ancestors(tx: &Transaction<'_>, folder: &FolderPath) -> Result<()> {
    let mut parent = Some(folder.clone());
    while let Some(folder) = parent {
        put_folder(tx, &folder)?;
        parent = folder.parent();
    }
    Ok(())
}

fn put_container(tx: &Transaction<'_>, c: &Container) -> Result<()> {
    put_ancestors(tx, &c.folder)?;
    let path = c.folder.child(c.name.as_str())?;
    tx.prepare_cached("INSERT INTO containers VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(id) DO UPDATE SET folder=excluded.folder,folder_key=excluded.folder_key,
            name=excluded.name,name_key=excluded.name_key,path_key=excluded.path_key,latest_blake3=excluded.latest_blake3,data=excluded.data")?
        .execute(params![c.id.as_str(), c.folder.as_str(), c.folder.key(), c.name.as_str(), c.name.key(), path.key(), c.latest_blake3.as_ref().ok_or(Error::Recovery)?.as_str(), rows::container_data(c)?])?;
    tx.prepare_cached("DELETE FROM tags WHERE id=?1")?
        .execute([c.id.as_str()])?;
    let mut insert = tx.prepare_cached("INSERT INTO tags VALUES(?1,?2)")?;
    for tag in &c.tags {
        insert.execute([c.id.as_str(), tag])?;
    }
    Ok(())
}

pub fn put_detail(tx: &Transaction<'_>, detail: &Detail) -> Result<()> {
    let c = &detail.container;
    paths::container_target(tx, &c.folder, &c.name, None)?;
    if rows::container(tx, FIND_CONTAINER, [c.id.as_str()])?.is_some() {
        return Err(invalid(format!("duplicate container ID {}", c.id)));
    }
    put_versions(tx, detail)
}

fn put_versions(tx: &Transaction<'_>, detail: &Detail) -> Result<()> {
    put_container(tx, &detail.container)?;
    for v in &detail.versions {
        put_version(tx, &detail.container.id, v)?;
    }
    Ok(())
}

fn put_version(tx: &Transaction<'_>, id: &ContainerId, version: &Version) -> Result<()> {
    tx.prepare_cached("INSERT INTO versions VALUES(?1,?2,?3) ON CONFLICT(id,version) DO UPDATE SET data=excluded.data")?
        .execute(params![id.as_str(), version.number.get(), json(version)?])?;
    Ok(())
}

pub fn put_recycle(tx: &Transaction<'_>, entry: &Recycled) -> Result<()> {
    tx.prepare_cached("INSERT OR REPLACE INTO recycle_bin VALUES(?1,?2,?3,?4)")?
        .execute(params![
            entry.recycle.recycle_id.as_str(),
            entry.recycle.recycled_at,
            entry.recycle_disk.as_str(),
            json(entry)?
        ])?;
    Ok(())
}

pub fn apply_change(
    tx: &Transaction<'_>,
    pending: &Pending,
    restored: Option<&Detail>,
    tree_entries: &[Recycled],
) -> Result<()> {
    match &pending.change {
        Change::Publish { commit } => {
            put_container(tx, &commit.container)?;
            put_version(tx, &commit.container.id, &commit.version)
        }
        Change::Patch { container, .. } => put_container(tx, container),
        Change::Recycle { entry, .. } => {
            detach(tx, &pending.change)?;
            put_recycle(tx, entry)
        }
        Change::RecycleTree { .. } => {
            detach(tx, &pending.change)?;
            for entry in tree_entries {
                put_recycle(tx, entry)?;
            }
            Ok(())
        }
        Change::Restore { entry, .. } => {
            tx.execute(
                "DELETE FROM recycle_bin WHERE id=?1",
                [entry.recycle.recycle_id.as_str()],
            )?;
            put_versions(tx, restored.ok_or(Error::Recovery)?)
        }
        Change::Prune {
            container,
            first_retained_version,
        } => {
            tx.execute(
                "DELETE FROM versions WHERE id=?1 AND version<?2",
                params![container.id.as_str(), first_retained_version.get()],
            )?;
            Ok(())
        }
        Change::CreateFolder { folder } => put_ancestors(tx, folder),
        Change::MoveFolder { from, to, .. } => move_folder(tx, from, to),
        Change::Purge { entry } => {
            tx.execute(
                "DELETE FROM recycle_bin WHERE id=?1",
                [entry.recycle.recycle_id.as_str()],
            )?;
            Ok(())
        }
        Change::PurgeAll { .. } => {
            tx.execute("DELETE FROM recycle_bin", [])?;
            Ok(())
        }
        Change::DeleteFolder { folder } => {
            tx.execute("DELETE FROM folders WHERE path_key=?1", [folder.key()])?;
            Ok(())
        }
    }
}

pub fn detach(tx: &Transaction<'_>, change: &Change) -> Result<()> {
    match change {
        Change::Recycle { entry, .. } => {
            tx.execute(
                "DELETE FROM containers WHERE id=?1",
                [entry.container.id.as_str()],
            )?;
        }
        Change::RecycleTree { folder, .. } => {
            let key = folder.key();
            let (start, end) = paths::descendants(&key);
            tx.execute(
                "DELETE FROM containers WHERE folder_key=?1 OR (folder_key>=?2 AND folder_key<?3)",
                params![key, start, end],
            )?;
            tx.execute(
                "DELETE FROM folders WHERE path_key=?1 OR (path_key>=?2 AND path_key<?3)",
                params![key, start, end],
            )?;
        }
        _ => return Err(Error::Recovery),
    }
    Ok(())
}

fn move_folder(tx: &Transaction<'_>, from: &FolderPath, to: &FolderPath) -> Result<()> {
    match paths::require_folder(tx, from) {
        Ok(()) => {}
        Err(Error::Missing(_) | Error::Conflict(_)) => return paths::require_folder(tx, to),
        Err(error) => return Err(error),
    }
    let parent = to.parent().ok_or(Error::Recovery)?;
    put_ancestors(tx, &parent)?;
    let key = from.key();
    let (start, end) = paths::descendants(&key);
    let to_key = to.key();
    let parent_key = parent.key();
    let parameters = params![
        key,
        from.as_str(),
        to.as_str(),
        to_key,
        start,
        end,
        parent_key
    ];
    tx.execute(MOVE_FOLDERS, parameters)?;
    tx.execute(MOVE_CONTAINERS, &parameters[..6])?;
    Ok(())
}
