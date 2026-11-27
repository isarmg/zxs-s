use crate::domain::FolderMove;

use super::{
    Change, Container, ContainerId, ContainerPage, Detail, Error, File, Filter, Patch, Pending,
    RecycleFilter, RecycleId, RecycleInfo, RecyclePage, Recycled, Restore, Result, State, Store,
    Version, VersionNum, hex, new_tx_id, normalize_tags, now, publication::Prepared, random,
    uploads::check_expected,
};

impl State {
    pub fn list(&self, filter: Filter) -> Result<ContainerPage> {
        self.healthy()?;
        self.db.containers(filter)
    }

    pub fn detail(&self, id: &ContainerId) -> Result<Detail> {
        self.healthy()?;
        self.db
            .detail(id)?
            .ok_or(Error::Missing("container not found"))
    }

    pub fn recycle_list(&self, filter: &RecycleFilter) -> Result<RecyclePage> {
        self.healthy()?;
        self.db.recycle(filter)
    }

    pub fn open_payload(&self, id: &ContainerId, version: VersionNum) -> Result<(Version, File)> {
        self.read_publication(|| {
            let container = self.container(id)?;
            let version = self
                .db
                .version(id, version)?
                .ok_or(Error::Missing("version not found"))?;
            let file = File::open(self.store.paths.payload(&container, &version))?;
            if file.metadata()?.len() != version.size {
                return Err(Error::Recovery);
            }
            Ok((version, file))
        })
    }

    pub fn patch(&self, id: &ContainerId, patch: Patch) -> Result<Container> {
        let tx_id = new_tx_id()?;
        let (previous, container, source, target, _write) = self.read_publication(|| {
            let previous = self.container(id)?;
            let (container, source, target) = patch_target(&previous, patch)?;
            self.available(&container.folder, &container.name, Some(id))?;
            let write = self.writes.admit(&tx_id, &[&source, &target])?;
            Ok((previous, container, source, target, write))
        })?;
        let mut stage = self.store.prepare_move(
            &FolderMove {
                from: source,
                to: target,
            },
            &tx_id,
            |copied| super::Store::check_history(copied, &self.indexed_history(&previous)?),
        )?;
        let pending = Pending {
            tx_id,
            change: Change::Patch {
                previous: previous.clone(),
                container: container.clone(),
                staged: stage.is_some(),
            },
        };
        let cleanup = self.serialize_publication(|| {
            if self.container(id)? != previous {
                return Err(Error::Conflict(
                    "container changed during move preparation".into(),
                ));
            }
            self.available(&container.folder, &container.name, Some(id))?;
            self.transact(&pending, stage.as_mut_slice(), Prepared::None)
        })?;
        Store::finish(cleanup);
        Ok(container)
    }

    pub fn recycle(&self, id: &ContainerId, expected: Option<VersionNum>) -> Result<Recycled> {
        let tx_id = new_tx_id()?;
        let (history, _write) = self.read_publication(|| {
            let history = self
                .db
                .detail(id)?
                .ok_or(Error::Missing("container not found"))?;
            let container = &history.container;
            if let Some(expected) = expected {
                check_expected(container.latest_version, expected)?;
            }
            let path = container.folder.child(container.name.as_str())?;
            let write = self.writes.admit(&tx_id, &[&path])?;
            Ok((history, write))
        })?;
        let container = &history.container;
        self.store.paths.data_disk(&container.folder)?;
        let recycle_disk = self
            .store
            .recycle_targets()?
            .select(history.payload_size())?;
        let entry = Recycled {
            recycle: RecycleInfo {
                recycle_id: RecycleId::parse(hex(random()?))?,
                recycled_at: now(),
            },
            container: container.into(),
            recycle_disk,
        };
        let mut stage = self.store.stage_recycle(&entry.recycle_disk, &tx_id)?;
        self.store.prepare_archive(&tx_id, &entry, &history)?;
        let pending = Pending {
            tx_id,
            change: Change::Recycle {
                source: container.folder.clone(),
                entry: entry.clone(),
            },
        };
        let cleanup = self.serialize_publication(|| {
            if self.container(id)? != *container {
                return Err(Error::Conflict(
                    "container changed during recycle preparation".into(),
                ));
            }
            self.transact(&pending, std::slice::from_mut(&mut stage), Prepared::None)
        })?;
        Store::finish(cleanup);
        Ok(entry)
    }

    pub fn restore(&self, recycle_id: &RecycleId, restore: Restore) -> Result<Container> {
        let tx_id = new_tx_id()?;
        let (entry, mut container, _write) = self.read_publication(|| {
            let entry = self.ready_recycle(recycle_id)?;
            let container = entry.container.restore(restore);
            self.available(&container.folder, &container.name, None)?;
            let path = container.folder.child(container.name.as_str())?;
            let write = self.writes.admit(&tx_id, &[&path])?;
            Ok((entry, container, write))
        })?;
        let disk = self.store.paths.data_disk(&container.folder)?;
        let (mut stage, versions) = self.store.prepare_restore(&entry, &disk, &tx_id)?;
        let (container, cleanup) = self.serialize_publication(|| {
            self.ready_recycle(recycle_id)?;
            self.available(&container.folder, &container.name, None)?;
            container.updated_at = now();
            container.latest_blake3 = versions.last().map(|version| version.blake3.clone());
            let pending = Pending {
                tx_id,
                change: Change::Restore {
                    container: container.clone(),
                    entry,
                },
            };
            let restored = Detail {
                container,
                versions,
            };
            let cleanup = self.transact(
                &pending,
                std::slice::from_mut(&mut stage),
                Prepared::Restore(&restored),
            )?;
            Ok((restored.container, cleanup))
        })?;
        Store::finish(cleanup);
        Ok(container)
    }

    fn ready_recycle(&self, id: &RecycleId) -> Result<Recycled> {
        self.db
            .recycled(id)?
            .ok_or(Error::Missing("recycle entry not found"))
    }

    pub fn purge(&self, id: Option<&RecycleId>) -> Result<u64> {
        let (count, cleanup) = self.serialize_publication(|| {
            let (change, count) = if let Some(id) = id {
                let entry = Box::new(self.ready_recycle(id)?);
                (Change::Purge { entry }, 1)
            } else {
                (
                    Change::PurgeAll {
                        disks: self
                            .store
                            .paths
                            .recycle_disks
                            .iter()
                            .map(|disk| disk.name.clone())
                            .collect(),
                    },
                    self.db.recycle_count()?,
                )
            };
            let pending = Pending {
                tx_id: new_tx_id()?,
                change,
            };
            Ok((count, self.transact(&pending, &mut [], Prepared::None)?))
        })?;
        Store::finish(cleanup);
        Ok(count)
    }
}

fn patch_target(
    previous: &Container,
    patch: Patch,
) -> Result<(Container, super::FolderPath, super::FolderPath)> {
    let mut container = previous.clone();
    if let Some(folder) = patch.folder {
        container.folder = folder;
    }
    if let Some(name) = patch.name {
        container.name = name;
    }
    if let Some(tags) = patch.tags {
        container.tags = normalize_tags(tags);
    }
    let source = previous.folder.child(previous.name.as_str())?;
    let destination = container.folder.child(container.name.as_str())?;
    let nested = |path: &str, parent: &str| {
        path.strip_prefix(parent)
            .is_some_and(|part| part.starts_with('/'))
    };
    if nested(source.as_str(), destination.as_str())
        || nested(destination.as_str(), source.as_str())
    {
        return Err(Error::Conflict(
            "a container cannot move into or over its own directory".into(),
        ));
    }
    container.updated_at = now();
    Ok((container, source, destination))
}
