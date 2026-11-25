use crate::domain::RecyclePlan;

use super::{
    Change, Container, ContainerId, ContainerPage, Detail, Error, File, Filter, Patch, Pending,
    RecycleFilter, RecycleId, RecycleInfo, RecyclePage, Recycled, Restore, Result, State, Version,
    VersionNum, hex, new_tx_id, normalize_tags, now, random, uploads::check_expected,
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
        let previous = self.read_publication(|| self.container(id))?;
        let container = patch_target(&previous, patch)?;
        self.available(&container.folder, &container.name, Some(id))?;
        let source = previous.folder.child(previous.name.as_str())?;
        let target = container.folder.child(container.name.as_str())?;
        let tx_id = new_tx_id()?;
        let mut stage = self.store.prepare_move(&source, &target, &tx_id)?;
        let pending = Pending {
            tx_id,
            change: Change::Patch {
                previous: previous.clone(),
                container: container.clone(),
                staged: stage.is_some(),
            },
        };
        self.serialize_publication(|| {
            if self.container(id)? != previous {
                return Err(Error::Conflict(
                    "container changed during move preparation".into(),
                ));
            }
            self.available(&container.folder, &container.name, Some(id))?;
            self.transact(&pending, stage.as_mut_slice())
        })?;
        self.complete(&pending);
        Ok(container)
    }

    pub fn recycle(&self, id: &ContainerId, expected: Option<VersionNum>) -> Result<Recycled> {
        let (container, bytes) = self.read_publication(|| {
            let container = self.container(id)?;
            if let Some(expected) = expected {
                check_expected(container.latest_version, expected)?;
            }
            Ok((container, self.db.payload_size(id)?))
        })?;
        let source_disk = self.store.paths.data_disk(&container.folder)?;
        let (recycle_disk, staged) = self.store.recycle_targets(&source_disk)?.select(bytes)?;
        let entry = Recycled {
            recycle: RecycleInfo {
                recycle_id: RecycleId::parse(hex(random()?))?,
                recycled_at: now(),
            },
            container,
            recycle_disk,
        };
        let tx_id = new_tx_id()?;
        let mut stage = self.store.stage_recycle(&entry.recycle_disk, &tx_id)?;
        self.store.prepare_archive(
            &tx_id,
            &RecyclePlan {
                entry: entry.clone(),
                staged,
            },
        )?;
        let pending = Pending {
            tx_id,
            change: Change::Recycle {
                entry: entry.clone(),
                staged,
            },
        };
        self.serialize_publication(|| {
            if self.container(id)? != entry.container {
                return Err(Error::Conflict(
                    "container changed during recycle preparation".into(),
                ));
            }
            self.transact(&pending, std::slice::from_mut(&mut stage))
        })?;
        self.complete(&pending);
        Ok(entry)
    }

    pub fn restore(&self, recycle_id: &RecycleId, restore: Restore) -> Result<Container> {
        let tx_id = new_tx_id()?;
        let entry = self.ready_recycle(recycle_id)?;
        let mut container = entry.container.clone();
        if let Some(folder) = restore.folder {
            container.folder = folder;
        }
        if let Some(name) = restore.name {
            container.name = name;
        }
        self.available(&container.folder, &container.name, None)?;
        let disk = self.store.paths.data_disk(&container.folder)?;
        let mut stage = self.store.prepare_restore(&entry, &disk, &tx_id)?;
        let (container, pending) = self.serialize_publication(|| {
            self.ready_recycle(recycle_id)?;
            self.available(&container.folder, &container.name, None)?;
            container.updated_at = now();
            let pending = Pending {
                tx_id,
                change: Change::Restore {
                    container: container.clone(),
                    entry,
                    staged: stage.is_some(),
                },
            };
            self.transact(&pending, stage.as_mut_slice())?;
            Ok((self.container(&container.id)?, pending))
        })?;
        self.complete(&pending);
        Ok(container)
    }

    fn ready_recycle(&self, id: &RecycleId) -> Result<Recycled> {
        self.db
            .recycled(id)?
            .ok_or(Error::Missing("recycle entry not found"))
    }

    pub fn purge(&self, id: Option<&RecycleId>) -> Result<u64> {
        let (count, pending) = self.serialize_publication(|| {
            let entry = id
                .map(|id| self.ready_recycle(id).map(Box::new))
                .transpose()?;
            let count = if entry.is_some() {
                1
            } else {
                self.db.recycle_count()?
            };
            let pending = Pending {
                tx_id: new_tx_id()?,
                change: Change::Purge { entry },
            };
            self.transact(&pending, &mut [])?;
            Ok((count, pending))
        })?;
        self.complete(&pending);
        Ok(count)
    }
}

fn patch_target(previous: &Container, patch: Patch) -> Result<Container> {
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
    Ok(container)
}
