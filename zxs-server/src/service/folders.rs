use std::collections::BTreeSet;

use crate::{
    domain::{DiskName, Error, Folder, FolderMove, FolderPage, Recycled, TxId, tree_recycle},
    store::Store,
};

use super::{
    Change, FolderPath, Pending, Result, StageGuard, State, invalid, new_tx_id, now,
    publication::Prepared, writes::FolderOperation,
};

impl State {
    pub fn folders(&self, folder: &FolderPath, recursive: bool) -> Result<FolderPage> {
        self.healthy()?;
        self.db.folders(folder, recursive)
    }

    pub fn edit_folder(&self, change: Change) -> Result<(Folder, bool)> {
        let tx_id = new_tx_id()?;
        let (folder, create) = match &change {
            Change::CreateFolder { folder } => (folder, true),
            Change::DeleteFolder { folder } => (folder, false),
            _ => return Err(invalid("expected a folder operation")),
        };
        self.folder_disk(folder)?;
        let _write = self.writes.admit(&tx_id, &[folder])?;
        let result = Folder {
            folder: folder.clone(),
        };
        let (created, cleanup) = self.serialize_publication(|| {
            if create {
                if self.db.has_folder(&result.folder)? {
                    return Ok((false, Vec::new()));
                }
                self.db.check_new_folder(&result.folder)?;
            } else {
                self.db.check_delete_folder(&result.folder)?;
            }
            let pending = Pending { tx_id, change };
            Ok((create, self.transact(&pending, &mut [], Prepared::None)?))
        })?;
        Store::finish(cleanup);
        Ok((result, created))
    }

    pub fn move_folder(&self, movement: FolderMove) -> Result<Folder> {
        let FolderMove { from, to } = &movement;
        let cross_disk = self.folder_disk(from)? != self.folder_disk(to)?;
        let tx_id = new_tx_id()?;
        let (write, frozen, mut stage) = if cross_disk {
            // Recycling the destination must wait for this move if it has already been admitted.
            let write = self.writes.admit(&tx_id, &[to])?;
            self.read_publication(|| {
                self.db.require_folder(from)?;
                self.db.check_new_folder(to)
            })?;
            let frozen = self.writes.freeze(from, FolderOperation::Move)?;
            let stage = self.copy_folder(&movement, &tx_id)?;
            (write, Some(frozen), Some(stage))
        } else {
            (self.writes.admit(&tx_id, &[from, to])?, None, None)
        };
        let FolderMove { from, to } = movement;
        let cleanup = self.serialize_publication(|| {
            if from == to {
                self.db.require_folder(&from)?;
                return Ok(Vec::new());
            }
            if cross_disk {
                self.db.check_new_folder(&to)?;
            } else {
                self.db.check_move_folder(&from, &to)?;
            }
            let pending = Pending {
                tx_id,
                change: Change::MoveFolder {
                    from,
                    to: to.clone(),
                    staged: cross_disk,
                },
            };
            self.transact(&pending, stage.as_mut_slice(), Prepared::None)
        })?;
        drop(frozen);
        drop(write);
        Store::finish(cleanup);
        Ok(Folder { folder: to })
    }

    fn copy_folder(&self, movement: &FolderMove, tx: &TxId) -> Result<StageGuard> {
        let containers = self.read_publication(|| {
            self.db.check_move_folder(&movement.from, &movement.to)?;
            self.db.subtree_containers(&movement.from)
        })?;
        let source = self.store.paths.folder(&movement.from);
        self.store.check_subtree(&source, &containers)?;
        self.store
            .prepare_move(movement, tx, |target| {
                for container in &containers {
                    let original = self.store.paths.container(container);
                    let relative = original
                        .strip_prefix(&source)
                        .map_err(|_| Error::Recovery)?;
                    let copied = target.join(relative);
                    Store::check_history(&copied, &self.indexed_history(container)?)?;
                }
                Ok(())
            })?
            .ok_or(Error::Recovery)
    }

    fn prepare_recycle_tree(
        &self,
        folder: &FolderPath,
        tx: &TxId,
    ) -> Result<(Vec<StageGuard>, Vec<Recycled>)> {
        let containers = self.read_publication(|| {
            self.db.require_folder(folder)?;
            self.db.subtree_containers(folder)
        })?;
        self.store
            .check_subtree(&self.store.paths.folder(folder), &containers)?;
        let mut selection = self.store.recycle_targets()?;
        let mut stages = Vec::new();
        let mut disks = BTreeSet::new();
        let mut archives = Vec::with_capacity(containers.len());
        let at = now();
        for container in containers {
            let history = self.indexed_history(&container)?;
            let disk = selection.select(history.payload_size())?;
            if disks.insert(disk.clone()) {
                stages.push(self.store.stage_recycle(&disk, tx)?);
            }
            let entry = tree_recycle(tx, at, &container, disk)?;
            self.store.prepare_archive(tx, &entry, &history)?;
            archives.push(entry);
        }
        stages.push(self.store.stage_plan(tx, &archives)?);
        Ok((stages, archives))
    }

    fn folder_disk(&self, folder: &FolderPath) -> Result<DiskName> {
        folder.require_child()?;
        self.store.paths.data_disk(folder)
    }

    pub fn remove_folder(&self, folder: FolderPath, recursive: bool) -> Result<Folder> {
        if !recursive {
            return Ok(self.edit_folder(Change::DeleteFolder { folder })?.0);
        }
        self.folder_disk(&folder)?;
        self.read_publication(|| self.db.require_folder(&folder))?;
        let frozen = self.writes.freeze(&folder, FolderOperation::Recycle)?;
        let tx_id = new_tx_id()?;
        let (mut stages, archives) = self.prepare_recycle_tree(&folder, &tx_id)?;
        let pending = Pending {
            tx_id,
            change: Change::RecycleTree {
                folder: folder.clone(),
            },
        };
        let cleanup = self.serialize_publication(|| {
            self.transact(&pending, &mut stages, Prepared::Archives(&archives))
        })?;
        drop(frozen);
        Store::finish(cleanup);
        Ok(Folder { folder })
    }
}
