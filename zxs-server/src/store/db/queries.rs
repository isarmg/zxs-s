use std::fs;

use rusqlite::{Connection, Params, params};

use super::{
    COUNT_RECYCLE, Db, FIND_CONTAINER, FIND_PAYLOAD, FIND_RECYCLED, INDEXED_ARCHIVES,
    INDEXED_VERSIONS, LIST_DESCENDANTS, LIST_FOLDERS, LIST_RECYCLED, LOOKUP_CONTAINERS,
    SUBTREE_CONTAINERS, container_listing, paths, rows,
};
use crate::{
    domain::{
        Container, ContainerCursor, ContainerId, ContainerPage, Detail, DiskName, Error, FileName,
        Filter, Folder, FolderPage, FolderPath, RecycleCursor, RecycleFilter, RecycleId,
        RecycleInfo, RecyclePage, Recycled, Result, Version, VersionNum, invalid, text_key,
    },
    store::Layout,
};

pub(super) fn compare_indexed_paths(
    paths: &Layout,
    index: &Connection,
    rebuilt: Option<&Connection>,
) -> Result<()> {
    check_versions(paths, index, rebuilt)?;
    check_archives(paths, index, rebuilt)
}

fn check_versions(paths: &Layout, index: &Connection, rebuilt: Option<&Connection>) -> Result<()> {
    let mut rebuilt = rebuilt
        .map(|index| {
            index.prepare(
                "SELECT EXISTS(SELECT 1 FROM containers c JOIN versions v ON c.id=v.id
                 WHERE c.path_key=?1 AND c.folder=?2 AND c.name=?3 AND v.version=?4)",
            )
        })
        .transpose()?;
    let mut versions = index.prepare(INDEXED_VERSIONS)?;
    let mut rows = versions.query([])?;
    while let Some(row) = rows.next()? {
        let folder = FolderPath::parse(row.get::<_, String>(0)?)?;
        let disk = folder.disk()?;
        if !paths.data_disks.iter().any(|online| online.name == disk) {
            continue;
        }
        let name = FileName::parse(row.get::<_, String>(1)?)?;
        let number = VersionNum::new(row.get(2)?)?;
        let version = || Layout::version(&paths.container_at(&folder, &name), number);
        let present = if let Some(query) = &mut rebuilt {
            query.query_row(
                params![
                    row.get::<_, String>(3)?,
                    folder.as_str(),
                    name.as_str(),
                    number.get()
                ],
                |row| row.get(0),
            )?
        } else {
            fs::symlink_metadata(version()).is_ok_and(|meta| meta.is_dir())
        };
        if !present {
            return Err(Error::IncompleteHistory(version()));
        }
    }
    Ok(())
}

fn check_archives(paths: &Layout, index: &Connection, rebuilt: Option<&Connection>) -> Result<()> {
    let mut rebuilt = rebuilt
        .map(|index| {
            index.prepare(
                "SELECT EXISTS(SELECT 1 FROM recycle_bin WHERE id=?1 AND recycled_at=?2 AND recycle_disk=?3)",
            )
        })
        .transpose()?;
    let mut archives = index.prepare(INDEXED_ARCHIVES)?;
    let mut rows = archives.query([])?;
    while let Some(row) = rows.next()? {
        let disk = DiskName::parse(row.get::<_, String>(2)?)?;
        if paths.recycle_disks.iter().any(|online| online.name == disk) {
            let recycle = RecycleInfo {
                recycle_id: RecycleId::parse(row.get::<_, String>(0)?)?,
                recycled_at: row.get(1)?,
            };
            let manifest = || Layout::metadata(&paths.recycle_batch(&disk, &recycle));
            let present = if let Some(query) = &mut rebuilt {
                query.query_row(
                    params![
                        recycle.recycle_id.as_str(),
                        recycle.recycled_at,
                        disk.as_str()
                    ],
                    |row| row.get(0),
                )?
            } else {
                fs::symlink_metadata(manifest()).is_ok_and(|meta| meta.is_file())
            };
            if !present {
                return Err(Error::IncompleteHistory(manifest()));
            }
        }
    }
    Ok(())
}

impl Db {
    pub fn container(&self, id: &ContainerId) -> Result<Option<Container>> {
        rows::container(&*self.read()?, FIND_CONTAINER, [id.as_str()])
    }

    pub fn payload(
        &self,
        id: &ContainerId,
        version: VersionNum,
    ) -> Result<(FolderPath, FileName, Version)> {
        let read = self.read()?;
        let mut query = read.prepare_cached(FIND_PAYLOAD)?;
        let mut records = query.query(params![id.as_str(), version.get()])?;
        let row = records
            .next()?
            .ok_or(Error::Missing("container not found"))?;
        let data: Option<String> = row.get(2)?;
        let data = data.ok_or(Error::Missing("version not found"))?;
        let (folder, name) = rows::location(row)?;
        Ok((folder, name, serde_json::from_str(&data)?))
    }

    pub fn detail(&self, id: &ContainerId) -> Result<Option<Detail>> {
        self.detail_checked(id, |_| Ok(()))
    }

    pub fn detail_checked(
        &self,
        id: &ContainerId,
        check: impl FnOnce(&Container) -> Result<()>,
    ) -> Result<Option<Detail>> {
        rows::detail(&*self.read()?, id, check)
    }

    pub fn recycled(&self, id: &RecycleId) -> Result<Option<Recycled>> {
        rows::one(&*self.read()?, FIND_RECYCLED, [id.as_str()])
    }

    pub fn containers(&self, filter: Filter) -> Result<ContainerPage> {
        if let Some(ids) = &filter.ids {
            return self.lookup_containers(ids, &filter);
        }
        if filter.recursive && filter.folder.is_none() {
            return Err(invalid("recursive container listing requires a folder"));
        }
        let folder = filter
            .folder
            .as_ref()
            .map_or_else(|| "/".into(), FolderPath::key);
        let (first, end) = if filter.recursive || filter.folder.is_none() {
            paths::descendants(&folder)
        } else {
            (folder.clone(), folder.clone())
        };
        let query = filter.q.map(|query| text_key(&query));
        let cursor = filter.cursor.as_ref();
        let cursor_folder = cursor.map(|cursor| cursor.folder.key());
        let cursor_folder = cursor_folder.as_deref().unwrap_or(&folder);
        if cursor_folder != folder
            && (cursor_folder < first.as_str() || cursor_folder >= end.as_str())
        {
            return Err(invalid("cursor does not belong to this directory"));
        }
        let (limit, read_limit) = page_limits(filter.limit);
        let read = self.read()?;
        let mut statement =
            read.prepare_cached(container_listing(&read, filter.tag.as_deref())?)?;
        let mut result = statement.query(params![
            cursor_folder,
            cursor.map_or_default(|c| c.name.key()),
            cursor.map_or("", |c| c.id.as_str()),
            folder,
            filter.tag,
            query,
            read_limit,
            first,
            end,
        ])?;
        let mut containers = Vec::new();
        while let Some(row) = result.next()? {
            containers.push(rows::container_row(row)?);
        }
        let more = containers.len() > limit;
        containers.truncate(limit);
        let next_cursor = containers
            .last()
            .filter(|_| more)
            .map(ContainerCursor::encode)
            .transpose()?;
        Ok(ContainerPage {
            containers,
            next_cursor,
        })
    }

    fn lookup_containers(&self, ids: &str, filter: &Filter) -> Result<ContainerPage> {
        const MAX_IDS: usize = 100;
        if filter.folder.is_some()
            || filter.recursive
            || filter.q.is_some()
            || filter.tag.is_some()
            || filter.cursor.is_some()
            || filter.limit.is_some()
        {
            return Err(invalid(
                "ids cannot be combined with other container filters",
            ));
        }
        let ids = ids
            .split(',')
            .take(MAX_IDS + 1)
            .map(ContainerId::parse)
            .collect::<Result<Vec<_>>>()?;
        if ids.len() > MAX_IDS {
            return Err(invalid("lookup accepts at most 100 container ids"));
        }
        let encoded = serde_json::to_string(&ids)?;
        let read = self.read()?;
        let mut statement = read.prepare_cached(LOOKUP_CONTAINERS)?;
        let mut result = statement.query([encoded])?;
        let mut containers = Vec::new();
        while let Some(row) = result.next()? {
            containers.push(rows::container_row(row)?);
        }
        Ok(ContainerPage {
            containers,
            next_cursor: None,
        })
    }

    pub fn folders(&self, parent: &FolderPath, recursive: bool) -> Result<FolderPage> {
        let read = self.read()?;
        let tx = read.unchecked_transaction()?;
        paths::require_folder(&tx, parent)?;
        let key = parent.key();
        let (first, last) = paths::descendants(&key);
        let mut statement = tx.prepare_cached(if recursive {
            LIST_DESCENDANTS
        } else {
            LIST_FOLDERS
        })?;
        let mut result = if recursive {
            statement.query([key, first, last])?
        } else {
            statement.query([key])?
        };
        let mut folders = Vec::new();
        while let Some(row) = result.next()? {
            folders.push(Folder {
                folder: FolderPath::parse(row.get::<_, String>(0)?)?,
            });
        }
        Ok(FolderPage { folders })
    }

    pub fn recycle(&self, filter: &RecycleFilter) -> Result<RecyclePage> {
        let (limit, read_limit) = page_limits(filter.limit);
        let (at, id) = match &filter.cursor {
            Some(cursor) => (
                i64::try_from(cursor.at).map_err(|_| invalid("invalid recycle cursor time"))?,
                cursor.id.as_str(),
            ),
            None => (i64::MAX, ""),
        };
        let mut entries: Vec<Recycled> =
            rows::many(&*self.read()?, LIST_RECYCLED, params![at, id, read_limit])?;
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = entries
            .last()
            .filter(|_| more)
            .map(RecycleCursor::encode)
            .transpose()?;
        Ok(RecyclePage {
            entries,
            next_cursor,
        })
    }

    pub fn recycle_count(&self) -> Result<u64> {
        Ok(self
            .read()?
            .query_row(COUNT_RECYCLE, [], |row| row.get(0))?)
    }

    pub fn check_container(
        &self,
        folder: &FolderPath,
        name: &FileName,
        except: Option<&ContainerId>,
    ) -> Result<()> {
        let read = self.read()?;
        let tx = read.unchecked_transaction()?;
        paths::container_target(&tx, folder, name, except)
    }

    pub fn check_new_folder(&self, folder: &FolderPath) -> Result<()> {
        let read = self.read()?;
        paths::folder_target(&read, folder, None)
    }

    pub fn has_folder(&self, folder: &FolderPath) -> Result<bool> {
        match self.require_folder(folder) {
            Ok(()) => Ok(true),
            Err(Error::Missing(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn check_move_folder(&self, from: &FolderPath, to: &FolderPath) -> Result<()> {
        let read = self.read()?;
        paths::check_move(&read, from, to)
    }

    pub fn check_delete_folder(&self, folder: &FolderPath) -> Result<()> {
        let read = self.read()?;
        paths::check_delete(&read, folder)
    }

    pub fn subtree_containers(&self, folder: &FolderPath) -> Result<Vec<Container>> {
        let key = folder.key();
        let (first, last) = paths::descendants(&key);
        read_subtree_containers(&*self.read()?, params![key, first, last])
    }
}

fn read_subtree_containers(read: &Connection, args: impl Params) -> Result<Vec<Container>> {
    let mut statement = read.prepare_cached(SUBTREE_CONTAINERS)?;
    let mut rows = statement.query(args)?;
    let mut containers = Vec::new();
    while let Some(row) = rows.next()? {
        containers.push(rows::container_row(row)?);
    }
    Ok(containers)
}

fn page_limits(requested: Option<u64>) -> (usize, i64) {
    let requested = requested.unwrap_or(100);
    let items = usize::try_from(requested).unwrap_or(usize::MAX);
    // SQLite uses LIMIT -1 when the count cannot be represented by a signed value.
    let read_limit = i64::try_from(requested.saturating_add(1)).unwrap_or(-1);
    (items, read_limit)
}
