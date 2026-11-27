use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufReader, ErrorKind},
    path::{Path, PathBuf},
};

use crate::{
    domain::{
        Change, Container, Detail, DiskName, Error, FolderPath, Pending, Recycled, Result, TxId,
        Version,
    },
    store::{StageGuard, integrity::verified_versions},
};

use super::{
    Layout, RecycleManifest, Store, copy_tree, mkdir, parent, remove_dir, rename, require_container,
};
use crate::store::fs_ops::atomic_json;

impl Store {
    pub fn recover_archives(&self, pending: &Pending) -> Result<Vec<Recycled>> {
        if !matches!(pending.change, Change::RecycleTree { .. }) {
            return Ok(Vec::new());
        }
        let path = self.paths.tree_archive_plan(&pending.tx_id);
        Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?)
    }

    pub fn prepare_archive(&self, tx: &TxId, entry: &Recycled, expected: &Detail) -> Result<()> {
        let archive = Archive::new(&self.paths, tx, entry);
        let source = self.paths.container(&expected.container);
        let copy = || {
            require_container(&source, &entry.container.id)?;
            archive.prepare()?;
            copy_tree(&source, &archive.container)?;
            Self::check_history(&archive.container, expected)
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

    pub(super) fn apply_recycle(
        &self,
        folder: &FolderPath,
        entry: &Recycled,
        tx: &TxId,
    ) -> Result<Option<PathBuf>> {
        let archive = Archive::new(&self.paths, tx, entry);
        if archive.published(&self.paths)? {
            return Ok(None);
        }
        let source = self.paths.container_at(folder, &entry.container.name);
        let detached = self.detach_source(&source, &self.paths.data_disk(folder)?, tx)?;
        archive.publish()?;
        Ok(Some(detached))
    }

    pub(super) fn apply_recycle_tree(
        &self,
        folder: &FolderPath,
        tx: &TxId,
        entries: &[Recycled],
    ) -> Result<Option<PathBuf>> {
        let mut staged = Vec::new();
        for entry in entries {
            let archive = Archive::new(&self.paths, tx, entry);
            if !archive.published(&self.paths)? {
                staged.push(archive);
            }
        }
        // The first published archive proves the whole source tree was detached.
        let detached = if staged.len() == entries.len() {
            Some(self.detach_source(
                &self.paths.folder(folder),
                &self.paths.data_disk(folder)?,
                tx,
            )?)
        } else {
            None
        };
        for archive in staged {
            archive.publish()?;
        }
        Ok(detached)
    }

    pub(super) fn detach_source(
        &self,
        source: &Path,
        disk: &DiskName,
        tx: &TxId,
    ) -> Result<PathBuf> {
        let detached = self.paths.live_trash(disk, tx);
        if !detached.try_exists()? {
            rename(source, &detached)?;
        }
        Ok(detached)
    }

    pub fn finish(paths: Vec<PathBuf>) {
        use rayon::prelude::*;
        paths.into_par_iter().for_each(|path| {
            if let Err(error) = remove_dir(&path) {
                tracing::warn!(path = %path.display(), %error, "temporary storage cleanup deferred");
            }
        });
    }

    pub fn prepare_restore(
        &self,
        entry: &Recycled,
        disk: &DiskName,
        op: &TxId,
    ) -> Result<(StageGuard, Vec<Version>)> {
        let batch = self.paths.recycled_entry(entry);
        let source = Layout::recycled_container(&batch, &entry.container.name);
        require_container(&source, &entry.container.id)?;
        let stage = self.stage(disk, op)?;
        let copied = self.paths.staged_tree(disk, op);
        copy_tree(&source, &copied)?;
        let metadata = require_container(&copied, &entry.container.id)?;
        let versions = verified_versions(&copied, &metadata)?;
        if versions
            .last()
            .is_none_or(|version| version.number != entry.container.latest_version)
        {
            return Err(Error::IncompleteHistory(source));
        }
        Ok((stage, versions))
    }

    pub(super) fn install_restore(
        &self,
        container: &Container,
        entry: &Recycled,
        tx: &TxId,
    ) -> Result<Option<PathBuf>> {
        let disk = self.paths.data_disk(&container.folder)?;
        let destination = self.paths.container(container);
        let source = self.paths.staged_tree(&disk, tx);
        if destination.try_exists()? {
            if source.try_exists()? {
                return Err(Error::Recovery);
            }
            self.install_metadata(&destination, container, tx)?;
            return Ok(None);
        }
        require_container(&source, &container.id)?;
        mkdir(parent(&destination)?)?;
        let detached = self.trash_entry(entry, tx)?;
        rename(&source, &destination)?;
        self.install_metadata(&destination, container, tx)?;
        Ok(Some(detached))
    }

    pub(super) fn detach_all(&self, disks: &[DiskName], op: &TxId) -> Result<Vec<PathBuf>> {
        for disk in disks {
            self.paths.require_recycle_disk(disk)?;
        }
        let mut cleanup = BTreeSet::new();
        for disk in disks {
            for path in self.recycle_batches(disk, false)? {
                let (entry, _) = Self::read_recycled(disk, &path)?;
                cleanup.insert(self.trash_entry(&entry, op)?);
            }
        }
        Ok(cleanup.into_iter().collect())
    }

    pub(super) fn trash_entry(&self, entry: &Recycled, op: &TxId) -> Result<PathBuf> {
        self.paths.require_recycle_disk(&entry.recycle_disk)?;
        let source = self.paths.recycled_entry(entry);
        let destination = self.paths.trash_entry(op, entry);
        if !destination.try_exists()? {
            mkdir(parent(&destination)?)?;
            rename(&source, &destination)?;
        }
        Ok(self.paths.trash(&entry.recycle_disk, op))
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

    fn published(&self, layout: &Layout) -> Result<bool> {
        layout.require_recycle_disk(&self.entry.recycle_disk)?;
        if !self.final_path.try_exists()? {
            require_container(&self.container, &self.entry.container.id)?;
            return Ok(false);
        }
        let (stored, _) = Store::read_recycled(&self.entry.recycle_disk, &self.final_path)?;
        if stored.recycle.recycle_id != self.entry.recycle.recycle_id
            || stored.container.id != self.entry.container.id
        {
            return Err(Error::Recovery);
        }
        Ok(true)
    }

    fn prepare(&self) -> Result<()> {
        mkdir(&self.incoming)?;
        atomic_json(
            &Layout::metadata(&self.incoming),
            &RecycleManifest {
                recycle_id: self.entry.recycle.recycle_id.clone(),
                recycled_at: self.entry.recycle.recycled_at,
                original_name: self.entry.container.name.clone(),
            },
        )
    }

    fn publish(&self) -> Result<()> {
        rename(&self.incoming, &self.final_path)
    }
}
