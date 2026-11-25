use std::collections::BTreeSet;

use crate::{
    domain::{Error, Folder, FolderPage, RecyclePlan, TxId, tree_recycle},
    store::db::RecycleTreeSnapshot,
};

use super::{Change, FolderPath, Pending, Result, StageGuard, State, invalid, new_tx_id, now};

#[derive(Default)]
struct Preparation {
    snapshot: Option<RecycleTreeSnapshot>,
    stages: Vec<StageGuard>,
}

impl State {
    pub fn folders(&self, folder: &FolderPath) -> Result<FolderPage> {
        self.healthy()?;
        self.db.folders(folder)
    }

    pub fn edit_folder(&self, mut change: Change) -> Result<(Folder, bool)> {
        let tx_id = new_tx_id()?;
        let mut prepared = self.prepare_folder_change(&tx_id, &mut change)?;
        let (result, pending) = self.serialize_publication(|| {
            let (folder, created) = self.check_folder_change(&change, &prepared)?;
            if !created && matches!(change, Change::CreateFolder { .. }) {
                return Ok(((folder, false), None));
            }
            let pending = Pending { tx_id, change };
            self.transact(&pending, &mut prepared.stages)?;
            Ok(((folder, created), Some(pending)))
        })?;
        if let Some(pending) = &pending {
            self.complete(pending);
        }
        Ok(result)
    }

    fn prepare_folder_change(&self, tx: &TxId, change: &mut Change) -> Result<Preparation> {
        match change {
            Change::CreateFolder { folder } | Change::DeleteFolder { folder } => {
                self.check_disk_child(folder)?;
                Ok(Preparation::default())
            }
            Change::MoveFolder { from, to, staged } => {
                self.check_disk_child(from)?;
                self.check_disk_child(to)?;
                let snapshot = self.read_publication(|| {
                    self.db.check_move_folder(from, to)?;
                    if from.disk()? == to.disk()? {
                        return Ok(None);
                    }
                    Ok(Some(self.db.recycle_tree_snapshot(from)?))
                })?;
                let stage = self.store.prepare_move(from, to, tx)?;
                *staged = stage.is_some();
                Ok(Preparation {
                    snapshot,
                    stages: stage.into_iter().collect(),
                })
            }
            Change::RecycleTree { folder } => self.prepare_recycle_tree(folder, tx),
            _ => Err(invalid("expected a folder operation")),
        }
    }

    fn prepare_recycle_tree(&self, folder: &FolderPath, tx: &TxId) -> Result<Preparation> {
        self.check_disk_child(folder)?;
        let snapshot = self.read_publication(|| self.db.recycle_tree_snapshot(folder))?;
        let mut selection = self.store.recycle_targets(&folder.disk()?)?;
        let mut stages = vec![self.store.stage_plan(tx)?];
        let mut disks = BTreeSet::new();
        let at = now();
        for container in &snapshot.containers {
            let (disk, copy_required) = selection.select(self.db.payload_size(&container.id)?)?;
            if disks.insert(disk.clone()) {
                stages.push(self.store.stage_recycle(&disk, tx)?);
            }
            let plan = RecyclePlan {
                entry: tree_recycle(tx, at, container.clone(), disk)?,
                staged: copy_required,
            };
            self.store.prepare_archive(tx, &plan)?;
            self.store.note_archive(tx, &plan)?;
        }
        Ok(Preparation {
            snapshot: Some(snapshot),
            stages,
        })
    }

    fn check_folder_change(
        &self,
        change: &Change,
        prepared: &Preparation,
    ) -> Result<(Folder, bool)> {
        let (folder, created) = match change {
            Change::CreateFolder { folder } => {
                let exists = self.db.has_folder(folder)?;
                if !exists {
                    self.db.check_new_folder(folder)?;
                }
                (folder, !exists)
            }
            Change::MoveFolder { from, to, .. } => {
                self.db.check_move_folder(from, to)?;
                self.check_snapshot(from, prepared)?;
                (to, false)
            }
            Change::DeleteFolder { folder } => {
                self.db.check_delete_folder(folder)?;
                (folder, false)
            }
            Change::RecycleTree { folder } => {
                self.check_snapshot(folder, prepared)?;
                (folder, false)
            }
            _ => return Err(invalid("expected a folder operation")),
        };
        Ok((
            Folder {
                folder: folder.clone(),
            },
            created,
        ))
    }

    fn check_disk_child(&self, folder: &FolderPath) -> Result<()> {
        folder.require_child()?;
        self.store.paths.data_disk(folder)?;
        Ok(())
    }

    fn check_snapshot(&self, folder: &FolderPath, prepared: &Preparation) -> Result<()> {
        if let Some(expected) = &prepared.snapshot
            && &self.db.recycle_tree_snapshot(folder)? != expected
        {
            return Err(Error::Conflict(
                "directory changed during copy preparation".into(),
            ));
        }
        Ok(())
    }

    pub fn remove_folder(&self, folder: FolderPath, recursive: bool) -> Result<Folder> {
        let change = if recursive {
            Change::RecycleTree { folder }
        } else {
            Change::DeleteFolder { folder }
        };
        Ok(self.edit_folder(change)?.0)
    }
}
