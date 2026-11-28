use std::fs;

use rusqlite::{Connection, OpenFlags, Transaction};

use super::{Db, SCHEMA, rows, write_index};
use crate::{
    domain::{DiskName, Error, FileName, FolderPath, RecycleId, RecycleInfo, Result, VersionNum},
    store::Layout,
};

impl Db {
    pub fn rebuild(
        &self,
        compare: Option<&Layout>,
        build: impl FnOnce(&Transaction<'_>) -> Result<()>,
    ) -> Result<()> {
        let writer = self.writer.lock().map_err(|_| Error::Recovery)?;
        if compare.is_some() {
            // The separate WAL reader sees the previous commit during the rebuild.
            writer.execute_batch(
                "ATTACH DATABASE (SELECT file FROM pragma_database_list WHERE name='main') AS previous;",
            )?;
        }
        let result = write_index(&writer, |tx| {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(
                "DELETE FROM versions; DELETE FROM tags; DELETE FROM containers;
                 DELETE FROM folders; DELETE FROM recycle_bin;",
            )?;
            build(tx)?;
            if let Some(paths) = compare {
                compare_indexed_paths(paths, tx, true)?;
            }
            Ok(())
        });
        match compare {
            Some(_) => result.and(
                writer
                    .execute_batch("DETACH DATABASE previous;")
                    .map_err(Error::from),
            ),
            None => result,
        }
    }
}

const INDEXED_VERSIONS: &str =
    "SELECT c.folder,c.name,v.version FROM versions v JOIN containers c ON c.id=v.id";
const INDEXED_ARCHIVES: &str = "SELECT id,recycled_at,recycle_disk FROM recycle_bin";
const MISSING_VERSIONS: &str = "SELECT c.folder,c.name,v.version
    FROM previous.versions v JOIN previous.containers c ON c.id=v.id
    LEFT JOIN main.containers n ON n.path_key=c.path_key AND n.folder=c.folder AND n.name=c.name
    LEFT JOIN main.versions nv ON nv.id=n.id AND nv.version=v.version WHERE nv.version IS NULL";
const MISSING_ARCHIVES: &str = "SELECT a.id,a.recycled_at,a.recycle_disk FROM previous.recycle_bin a
    LEFT JOIN main.recycle_bin n ON n.id=a.id AND n.recycled_at=a.recycled_at AND n.recycle_disk=a.recycle_disk
    WHERE n.id IS NULL";

pub(in crate::store) fn check_indexed_paths(paths: &Layout) -> Result<()> {
    let path = paths.index();
    if path.try_exists()? {
        let index = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !rows::index_empty(&index)? {
            return compare_indexed_paths(paths, &index, false);
        }
    }
    Ok(())
}

fn compare_indexed_paths(paths: &Layout, index: &Connection, comparing: bool) -> Result<()> {
    check_versions(paths, index, comparing)?;
    check_archives(paths, index, comparing)
}

fn check_versions(paths: &Layout, index: &Connection, comparing: bool) -> Result<()> {
    let mut versions = index.prepare(if comparing {
        MISSING_VERSIONS
    } else {
        INDEXED_VERSIONS
    })?;
    let mut rows = versions.query([])?;
    while let Some(row) = rows.next()? {
        let folder = FolderPath::parse(row.get::<_, String>(0)?)?;
        let disk = folder.disk()?;
        if !paths.data_disks.iter().any(|online| online.name == disk) {
            continue;
        }
        let name = FileName::parse(row.get::<_, String>(1)?)?;
        let number = VersionNum::new(row.get(2)?)?;
        let version = Layout::version(&paths.container_at(&folder, &name), number);
        if comparing || !fs::symlink_metadata(&version).is_ok_and(|meta| meta.is_dir()) {
            return Err(Error::IncompleteHistory(version));
        }
    }
    Ok(())
}

fn check_archives(paths: &Layout, index: &Connection, comparing: bool) -> Result<()> {
    let mut archives = index.prepare(if comparing {
        MISSING_ARCHIVES
    } else {
        INDEXED_ARCHIVES
    })?;
    let mut rows = archives.query([])?;
    while let Some(row) = rows.next()? {
        let disk = DiskName::parse(row.get::<_, String>(2)?)?;
        if paths.recycle_disks.iter().any(|online| online.name == disk) {
            let recycle = RecycleInfo {
                recycle_id: RecycleId::parse(row.get::<_, String>(0)?)?,
                recycled_at: row.get(1)?,
            };
            let manifest = Layout::metadata(&paths.recycle_batch(&disk, &recycle));
            if comparing || !fs::symlink_metadata(&manifest).is_ok_and(|meta| meta.is_file()) {
                return Err(Error::IncompleteHistory(manifest));
            }
        }
    }
    Ok(())
}
