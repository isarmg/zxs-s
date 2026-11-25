use rusqlite::params;

use super::{
    COUNT_RECYCLE, Db, FIND_CONTAINER, FIND_RECYCLED, FIND_VERSION, FIND_VERSIONS, LIST_CONTAINERS,
    LIST_FOLDER_CONTAINERS, LIST_FOLDERS, LIST_RECYCLED, PAYLOAD_SIZE, SUBTREE_CONTAINERS,
    SUBTREE_FOLDERS, paths, rows,
};
use crate::domain::{
    Container, ContainerCursor, ContainerId, ContainerPage, Detail, Error, FileName, Filter,
    Folder, FolderPage, FolderPath, RecycleCursor, RecycleFilter, RecycleId, RecyclePage, Recycled,
    Result, Version, VersionNum, invalid,
};

#[derive(PartialEq, Eq)]
pub struct RecycleTreeSnapshot {
    folders: Vec<String>,
    pub containers: Vec<Container>,
}

impl Db {
    pub fn container(&self, id: &ContainerId) -> Result<Option<Container>> {
        rows::container(&*self.read()?, FIND_CONTAINER, [id.as_str()])
    }

    pub fn version(&self, id: &ContainerId, version: VersionNum) -> Result<Option<Version>> {
        rows::one(
            &*self.read()?,
            FIND_VERSION,
            params![id.as_str(), version.get()],
        )
    }

    pub fn payload_size(&self, id: &ContainerId) -> Result<u64> {
        Ok(self
            .read()?
            .prepare_cached(PAYLOAD_SIZE)?
            .query_row([id.as_str()], |row| row.get(0))?)
    }

    pub fn detail(&self, id: &ContainerId) -> Result<Option<Detail>> {
        let read = self.read()?;
        let tx = read.unchecked_transaction()?;
        let Some(container) = rows::container(&tx, FIND_CONTAINER, [id.as_str()])? else {
            return Ok(None);
        };
        let versions = rows::many(&tx, FIND_VERSIONS, [id.as_str()])?;
        Ok(Some(Detail {
            container,
            versions,
        }))
    }

    pub fn recycled(&self, id: &RecycleId) -> Result<Option<Recycled>> {
        rows::one(&*self.read()?, FIND_RECYCLED, [id.as_str()])
    }

    pub fn containers(&self, filter: Filter) -> Result<ContainerPage> {
        let folder = filter.folder.as_ref().map(FolderPath::key);
        let query = filter.q.map(|q| q.to_lowercase());
        let cursor = filter.cursor.as_ref();
        let (limit, read_limit) = page_limits(filter.limit);
        let read = self.read()?;
        let mut statement = read.prepare_cached(if let Some(folder) = &folder {
            if cursor.is_some_and(|cursor| cursor.folder.key() != *folder) {
                return Err(invalid("cursor does not belong to this folder"));
            }
            LIST_FOLDER_CONTAINERS
        } else {
            LIST_CONTAINERS
        })?;
        let mut result = statement.query(params![
            cursor.map(|c| c.folder.key()).unwrap_or_default(),
            cursor.map(|c| c.name.key()).unwrap_or_default(),
            cursor.map_or("", |c| c.id.as_str()),
            folder,
            filter.tag,
            query,
            read_limit
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

    pub fn folders(&self, parent: &FolderPath) -> Result<FolderPage> {
        let read = self.read()?;
        let tx = read.unchecked_transaction()?;
        paths::require_folder(&tx, parent)?;
        let mut statement = tx.prepare_cached(LIST_FOLDERS)?;
        let mut result = statement.query([parent.key()])?;
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
        let read = self.read()?;
        match paths::require_folder(&read, folder) {
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

    pub fn recycle_tree_snapshot(&self, folder: &FolderPath) -> Result<RecycleTreeSnapshot> {
        let read = self.read()?;
        paths::require_folder(&read, folder)?;
        let key = folder.key();
        let (first, last) = paths::descendants(&key);
        let args = params![key, first, last];
        let mut folders = read.prepare_cached(SUBTREE_FOLDERS)?;
        let folders = folders
            .query_map(args, |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut statement = read.prepare_cached(SUBTREE_CONTAINERS)?;
        let mut loaded = statement.query(args)?;
        let mut containers = Vec::new();
        while let Some(row) = loaded.next()? {
            containers.push(rows::container_row(row)?);
        }
        Ok(RecycleTreeSnapshot {
            folders,
            containers,
        })
    }
}

fn page_limits(requested: Option<u64>) -> (usize, i64) {
    let requested = requested.unwrap_or(100);
    let items = usize::try_from(requested).unwrap_or(usize::MAX);
    // SQLite uses LIMIT -1 when the count cannot be represented by a signed value.
    let read_limit = i64::try_from(requested.saturating_add(1)).unwrap_or(-1);
    (items, read_limit)
}
