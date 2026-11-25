use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::{
    domain::{
        Change, Container, DiskName, Error, FolderPath, Pending, RecyclePlan, Recycled, Result,
        TxId,
    },
    store::StageGuard,
};

use super::{
    Layout, RecycleManifest, Store, copy_tree, entries, mkdir, parent, read_json, remove_dir,
    rename, require_container,
};
use crate::store::fs_ops::atomic_json;

impl Store {
    pub fn archived_entries(&self, pending: &Pending) -> Result<Vec<Recycled>> {
        match &pending.change {
            Change::Recycle { entry, .. } => Ok(vec![entry.clone()]),
            Change::RecycleTree { .. } => Ok(self
                .tree_entries(&pending.tx_id)?
                .into_iter()
                .map(|item| item.entry)
                .collect()),
            _ => Ok(Vec::new()),
        }
    }

    fn tree_entries(&self, tx: &TxId) -> Result<Vec<RecyclePlan>> {
        entries(&self.paths.tree_archives(tx))?
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| read_json(path))
            .collect()
    }

    pub fn prepare_archive(&self, tx: &TxId, plan: &RecyclePlan) -> Result<()> {
        let entry = &plan.entry;
        let archive = Archive::new(&self.paths, tx, entry);
        let source = self.paths.container(&entry.container);
        let copy = || {
            require_container(&source, &entry.container.id)?;
            archive.prepare()?;
            if plan.staged {
                copy_tree(&source, &archive.container)?;
            }
            Ok(())
        };
        match copy() {
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => Err(Error::Conflict(
                "container changed during recycle preparation".into(),
            )),
            Err(Error::Io(error)) if error.kind() == ErrorKind::StorageFull => {
                Err(Error::RecycleFull)
            }
            result => result,
        }
    }

    pub(super) fn apply_recycle(&self, entry: &Recycled, tx: &TxId, staged: bool) -> Result<()> {
        let archive = Archive::new(&self.paths, tx, entry);
        let source = self.paths.container(&entry.container);
        if staged {
            archive.publish()?;
            self.detach_source(&source, &entry.container.folder.disk()?, tx)
        } else {
            archive.move_container(&source)
        }
    }

    pub(super) fn apply_recycle_tree(&self, folder: &FolderPath, tx: &TxId) -> Result<()> {
        let plans = self.tree_entries(tx)?;
        for plan in plans.iter().filter(|plan| plan.staged) {
            Archive::new(&self.paths, tx, &plan.entry).publish()?;
        }
        let disk = folder.disk()?;
        let source = self.paths.folder(folder);
        let detached = self.paths.live_trash(&disk, tx);
        self.detach_source(&source, &disk, tx)?;
        for plan in plans.iter().filter(|plan| !plan.staged) {
            let relative = self.paths.container(&plan.entry.container);
            let relative = relative
                .strip_prefix(&source)
                .map_err(|_| Error::Recovery)?;
            Archive::new(&self.paths, tx, &plan.entry).move_container(&detached.join(relative))?;
        }
        Ok(())
    }

    pub fn note_archive(&self, tx: &TxId, plan: &RecyclePlan) -> Result<()> {
        atomic_json(
            &self.paths.tree_archive(tx, &plan.entry.recycle.recycle_id),
            plan,
        )
    }

    pub(super) fn detach_source(&self, source: &Path, disk: &DiskName, tx: &TxId) -> Result<()> {
        let detached = self.paths.live_trash(disk, tx);
        if !detached.try_exists()? {
            rename(source, &detached)?;
        }
        Ok(())
    }

    pub fn finish(&self, pending: &Pending) -> Result<()> {
        use rayon::prelude::*;
        self.paths
            .transaction_spools(&pending.tx_id)
            .par_iter()
            .try_for_each(|path| remove_dir(path))
    }

    pub fn prepare_restore(
        &self,
        entry: &Recycled,
        disk: &DiskName,
        op: &TxId,
    ) -> Result<Option<StageGuard>> {
        let batch = self.paths.recycled_entry(entry);
        let source = Layout::recycled_container(&batch, &entry.container.name);
        if fs::metadata(&source)?.dev() == fs::metadata(self.paths.staging_root(disk))?.dev() {
            return Ok(None);
        }
        require_container(&source, &entry.container.id)?;
        let stage = self.stage(disk, op)?;
        copy_tree(&source, &self.paths.staged_tree(disk, op))?;
        Ok(Some(stage))
    }

    pub(super) fn install_restore(
        &self,
        container: &Container,
        entry: &Recycled,
        pending: &Pending,
        staged: bool,
    ) -> Result<()> {
        let archive = self.paths.recycled_entry(entry);
        let source = if staged {
            self.paths
                .staged_tree(&container.folder.disk()?, &pending.tx_id)
        } else {
            Layout::recycled_container(&archive, &entry.container.name)
        };
        let destination = self.paths.container(container);
        if !destination.try_exists()? {
            require_container(&source, &container.id)?;
            mkdir(parent(&destination)?)?;
            rename(&source, &destination)?;
        }
        require_container(&destination, &container.id)?;
        self.install_metadata(&destination, container, &pending.tx_id)?;
        self.detach_purge(Some(entry), &pending.tx_id)?;
        Ok(())
    }

    pub(super) fn detach_purge(&self, entry: Option<&Recycled>, op: &TxId) -> Result<()> {
        if let Some(entry) = entry {
            return self.trash_entry(entry, op);
        }
        for (disk, path) in self.recycle_paths()? {
            self.trash_entry(&Self::read_recycled(&disk, &path)?, op)?;
        }
        Ok(())
    }

    fn trash_entry(&self, entry: &Recycled, op: &TxId) -> Result<()> {
        let source = self.paths.recycled_entry(entry);
        let destination = self.paths.trash_entry(op, entry);
        if destination.try_exists()? {
            return Ok(());
        }
        mkdir(parent(&destination)?)?;
        rename(&source, &destination)
    }
}

struct Archive<'a> {
    entry: &'a Recycled,
    incoming: PathBuf,
    final_path: PathBuf,
    container: PathBuf,
}

impl<'a> Archive<'a> {
    fn new(layout: &Layout, op: &TxId, entry: &'a Recycled) -> Self {
        let incoming = layout.incoming_entry(&entry.recycle_disk, op, &entry.recycle.recycle_id);
        Self {
            entry,
            container: Layout::recycled_container(&incoming, &entry.container.name),
            incoming,
            final_path: layout.recycled_entry(entry),
        }
    }

    fn finalized(&self) -> Result<bool> {
        if !self.final_path.try_exists()? {
            return Ok(false);
        }
        let stored = Store::read_recycled(&self.entry.recycle_disk, &self.final_path)?;
        if stored.recycle.recycle_id != self.entry.recycle.recycle_id
            || stored.container.id != self.entry.container.id
        {
            return Err(Error::Recovery);
        }
        Ok(true)
    }

    fn prepare(&self) -> Result<()> {
        let manifest = Layout::recycle_meta(&self.incoming);
        mkdir(parent(&manifest)?)?;
        atomic_json(
            &manifest,
            &RecycleManifest {
                recycle_id: self.entry.recycle.recycle_id.clone(),
                recycled_at: self.entry.recycle.recycled_at,
                original_folder: self.entry.container.folder.clone(),
                original_name: self.entry.container.name.clone(),
            },
        )
    }

    fn publish(&self) -> Result<()> {
        if self.finalized()? {
            return Ok(());
        }
        require_container(&self.container, &self.entry.container.id)?;
        rename(&self.incoming, &self.final_path)
    }

    fn move_container(&self, source: &Path) -> Result<()> {
        if self.finalized()? {
            return Ok(());
        }
        if !self.container.try_exists()? {
            if !fs::symlink_metadata(Layout::recycle_meta(&self.incoming))?.is_file() {
                return Err(Error::Recovery);
            }
            require_container(source, &self.entry.container.id)?;
            rename(source, &self.container)?;
        }
        self.publish()
    }
}
