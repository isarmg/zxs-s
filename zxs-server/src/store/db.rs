mod paths;
mod queries;
mod rebuild;
mod rows;
pub use paths::put_folder;
pub(super) use rebuild::check_indexed_paths;
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

use crate::domain::{
    Change, Commit, Container, ContainerId, Detail, Error, Pending, Recycled, Result, Version,
    VersionNum, invalid,
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
const FIND_PAYLOAD: &str = "SELECT c.folder,c.name,v.data FROM containers c LEFT JOIN versions v
    ON v.id=c.id AND v.version=?2 WHERE c.id=?1";
const FIND_VERSIONS: &str = "SELECT data FROM versions WHERE id=?1 ORDER BY version";
const FIND_RECYCLED: &str = "SELECT data FROM recycle_bin WHERE id=?1";
const LIST_RECYCLED: &str = "SELECT data FROM recycle_bin WHERE (recycled_at,id)<(?1,?2) ORDER BY recycled_at DESC,id DESC LIMIT ?3";
const FIND_FOLDER: &str = "SELECT path FROM folders WHERE path_key=?1";
const FIND_PATH: &str = "SELECT id,folder||'/'||name FROM containers WHERE path_key=?1";
macro_rules! container_listing_sql {
    ($from:literal, $tag:literal) => {
        concat!(
            "SELECT folder,name,latest_blake3,data FROM ",
            $from,
            " WHERE (folder_key,name_key,c.id)>(?1,?2,?3) AND folder_key<=?9",
            " AND (folder_key=?4 OR (folder_key>=?8 AND folder_key<?9)) AND ",
            $tag,
            " AND (?6 IS NULL OR instr(name_key,?6)>0) ORDER BY folder_key,name_key,c.id LIMIT ?7"
        )
    };
}
// The cursor supplies the lower index bound, and the separate upper bound ends the scan.
const LIST_CONTAINERS: &str = container_listing_sql!(
    "containers AS c",
    "(?5 IS NULL OR EXISTS(SELECT 1 FROM tags WHERE id=c.id AND tag=?5))"
);
const LIST_TAGGED_CONTAINERS: &str =
    container_listing_sql!("tags AS t JOIN containers AS c ON c.id=t.id", "t.tag=?5");
const LIST_FOLDERS: &str = "SELECT path FROM folders WHERE parent_key=?1 ORDER BY path_key";
const LIST_DESCENDANTS: &str =
    "SELECT path FROM folders WHERE path_key>?1 AND path_key>=?2 AND path_key<?3 ORDER BY path_key";
const LOOKUP_CONTAINERS: &str = "
    SELECT folder,name,latest_blake3,data FROM containers
    WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY folder_key,name_key,id";
const SUBTREE_CONTAINERS: &str = "SELECT folder,name,latest_blake3,data,folder_key,name_key,id FROM containers WHERE folder_key=?1
    UNION ALL SELECT folder,name,latest_blake3,data,folder_key,name_key,id FROM containers WHERE folder_key>=?2 AND folder_key<?3
    ORDER BY folder_key,name_key,id";
const FOLDER_OCCUPIED: &str = "SELECT EXISTS(SELECT 1 FROM containers WHERE folder_key=?1)
    OR EXISTS(SELECT 1 FROM folders WHERE path_key>=?2 AND path_key<?3)";
const LONGEST_PATH: &str = "SELECT max(n) FROM (
    SELECT max(length(CAST(path AS BLOB))) AS n FROM folders WHERE path_key=?1 OR (path_key>=?2 AND path_key<?3)
    UNION ALL SELECT max(length(CAST(folder||'/'||name AS BLOB))) FROM containers
        WHERE folder_key=?1 OR (folder_key>=?2 AND folder_key<?3))";

fn container_listing(read: &Connection, tag: Option<&str>) -> Result<&'static str> {
    // Bound the probe and the number of results that a tag-first listing has to sort.
    if let Some(tag) = tag
        && !read
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM tags WHERE tag=?1 LIMIT 1 OFFSET 64)")?
            .query_row([tag], |row| row.get::<_, bool>(0))?
    {
        return Ok(LIST_TAGGED_CONTAINERS);
    }
    Ok(LIST_CONTAINERS)
}

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
        let empty = rows::index_empty(&writer)?;
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

    pub fn write(&self, change: impl FnOnce(&Transaction<'_>) -> Result<()>) -> Result<()> {
        let writer = self.writer.lock().map_err(|_| Error::Recovery)?;
        write_index(&writer, change)
    }

    fn read(&self) -> Result<MutexGuard<'_, Connection>> {
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        rows::available_reader(&self.readers, start)
    }
}

fn write_index(
    writer: &Connection,
    change: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<()> {
    let tx = writer.unchecked_transaction()?;
    change(&tx)?;
    tx.commit()?;
    Ok(())
}

fn put_container(tx: &Transaction<'_>, c: &Container, update: bool) -> Result<()> {
    let path = c.folder.child(c.name.as_str())?;
    let changed = tx.prepare_cached("INSERT INTO containers VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(id) DO UPDATE SET folder=excluded.folder,folder_key=excluded.folder_key,
            name=excluded.name,name_key=excluded.name_key,path_key=excluded.path_key,latest_blake3=excluded.latest_blake3,data=excluded.data WHERE ?9")?
        .execute(params![c.id.as_str(), c.folder.as_str(), c.folder.key(), c.name.as_str(), c.name.key(), path.key(), c.latest_blake3.as_ref().ok_or(Error::Recovery)?.as_str(), rows::container_data(c)?, update])?;
    if changed == 0 {
        return Err(invalid(format!("duplicate container ID {}", c.id)));
    }
    Ok(())
}

fn put_tags(tx: &Transaction<'_>, c: &Container) -> Result<()> {
    let mut insert = tx.prepare_cached("INSERT INTO tags VALUES(?1,?2)")?;
    for tag in &c.tags {
        insert.execute([c.id.as_str(), tag])?;
    }
    Ok(())
}

fn put_container_with_ancestors(tx: &Transaction<'_>, c: &Container) -> Result<()> {
    paths::put_ancestors(tx, &c.folder)?;
    put_container(tx, c, true)?;
    put_tags(tx, c)
}

pub fn put_detail(tx: &Transaction<'_>, detail: &Detail) -> Result<()> {
    let c = &detail.container;
    // The rebuild has already inserted every directory the scan found.
    paths::container_slot(tx, &c.folder.child(c.name.as_str())?, None)?;
    put_container(tx, c, false)?;
    put_tags(tx, c)?;
    put_versions(tx, detail)
}

fn put_versions(tx: &Transaction<'_>, detail: &Detail) -> Result<()> {
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

fn put_commit(tx: &Transaction<'_>, commit: &Commit) -> Result<()> {
    let c = &commit.container;
    if commit.version.number == VersionNum::FIRST {
        put_container_with_ancestors(tx, c)?;
    } else {
        tx.prepare_cached("UPDATE containers SET latest_blake3=?1,data=?2 WHERE id=?3")?
            .execute(params![
                c.latest_blake3.as_ref().ok_or(Error::Recovery)?.as_str(),
                rows::container_data(c)?,
                c.id.as_str()
            ])?;
    }
    put_version(tx, &c.id, &commit.version)
}

fn remove_recycle(tx: &Transaction<'_>, entry: &Recycled) -> Result<()> {
    tx.execute(
        "DELETE FROM recycle_bin WHERE id=?1",
        [entry.recycle.recycle_id.as_str()],
    )?;
    Ok(())
}

pub fn apply_change(
    tx: &Transaction<'_>,
    pending: &Pending,
    restored: Option<&Detail>,
    tree_entries: &[Recycled],
) -> Result<()> {
    match &pending.change {
        Change::Publish { commit } => put_commit(tx, commit),
        Change::Patch {
            previous,
            container,
            ..
        } => {
            if previous.folder != container.folder {
                paths::put_ancestors(tx, &container.folder)?;
            }
            put_container(tx, container, true)?;
            if previous.tags != container.tags {
                tx.prepare_cached("DELETE FROM tags WHERE id=?1")?
                    .execute([container.id.as_str()])?;
                put_tags(tx, container)?;
            }
            Ok(())
        }
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
            let detail = restored.ok_or(Error::Recovery)?;
            remove_recycle(tx, entry)?;
            put_container_with_ancestors(tx, &detail.container)?;
            put_versions(tx, detail)
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
        Change::CreateFolder { folder } => paths::put_ancestors(tx, folder),
        Change::MoveFolder { from, to, .. } => paths::move_folder(tx, from, to),
        Change::Purge { entry } => remove_recycle(tx, entry),
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
