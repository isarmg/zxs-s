use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::{
    domain::{
        Change, Container, DiskName, Error, FolderPath, Pending, RecyclePlan, Recycled, Result,
        TxId, Version,
    },
    store::{StageGuard, integrity::verified_versions},
};

use super::{
    Layout, RecycleManifest, Store, copy_tree, entries, mkdir, parent, read_json, remove_dir,
    rename, require_container,
};
use crate::store::fs_ops::atomic_json;

impl Store {
    pub fn recover_archives(&self, pending: &Pending) -> Result<Vec<RecyclePlan>> {
        if !matches!(pending.change, Change::RecycleTree { .. }) {
            return Ok(Vec::new());
        }
        entries(&self.paths.tree_archives(&pending.tx_id))?
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| read_json(path))
            .collect()
    }

    pub fn prepare_archive(
        &self,
        tx: &TxId,
        plan: &RecyclePlan,
        expected: &crate::domain::Detail,
    ) -> Result<()> {
        let entry = &plan.entry;
        let archive = Archive::new(&self.paths, tx, entry);
        let source = self.paths.container(&entry.container);
        let copy = || {
            require_container(&source, &entry.container.id)?;
            archive.prepare()?;
            let checked = if plan.staged {
                copy_tree(&source, &archive.container)?;
                &archive.container
            } else {
                &source
            };
            Self::check_history(checked, expected, plan.staged)
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
        entry: &Recycled,
        tx: &TxId,
        staged: bool,
    ) -> Result<Option<PathBuf>> {
        let archive = Archive::new(&self.paths, tx, entry);
        let source = self.paths.container(&entry.container);
        if staged {
            archive.publish()?;
            return self
                .detach_source(&source, &entry.container.folder.disk()?, tx)
                .map(Some);
        }
        archive.move_container(&source)?;
        Ok(None)
    }

    pub(super) fn apply_recycle_tree(
        &self,
        folder: &FolderPath,
        tx: &TxId,
        plans: &[RecyclePlan],
    ) -> Result<PathBuf> {
        for plan in plans.iter().filter(|plan| plan.staged) {
            Archive::new(&self.paths, tx, &plan.entry).publish()?;
        }
        let disk = folder.disk()?;
        let source = self.paths.folder(folder);
        let detached = self.detach_source(&source, &disk, tx)?;
        for plan in plans.iter().filter(|plan| !plan.staged) {
            let relative = self.paths.container(&plan.entry.container);
            let relative = relative
                .strip_prefix(&source)
                .map_err(|_| Error::Recovery)?;
            Archive::new(&self.paths, tx, &plan.entry).move_container(&detached.join(relative))?;
        }
        Ok(detached)
    }

    pub fn note_archive(&self, tx: &TxId, plan: &RecyclePlan) -> Result<()> {
        atomic_json(
            &self.paths.tree_archive(tx, &plan.entry.recycle.recycle_id),
            plan,
        )
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
    ) -> Result<(Option<StageGuard>, Vec<Version>)> {
        let batch = self.paths.recycled_entry(entry);
        let source = Layout::recycled_container(&batch, &entry.container.name);
        let original = require_container(&source, &entry.container.id)?;
        let (stage, versions) =
            if fs::metadata(&source)?.dev() == fs::metadata(self.paths.staging_root(disk))?.dev() {
                (None, verified_versions(&source, &original)?)
            } else {
                let stage = self.stage(disk, op)?;
                let copied = self.paths.staged_tree(disk, op);
                copy_tree(&source, &copied)?;
                let metadata = require_container(&copied, &entry.container.id)?;
                (Some(stage), verified_versions(&copied, &metadata)?)
            };
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
        staged: bool,
    ) -> Result<PathBuf> {
        let archive = self.paths.recycled_entry(entry);
        let source = if staged {
            self.paths.staged_tree(&container.folder.disk()?, tx)
        } else {
            Layout::recycled_container(&archive, &entry.container.name)
        };
        let destination = self.paths.container(container);
        if destination.try_exists()? {
            require_container(&destination, &container.id)?;
        } else {
            require_container(&source, &container.id)?;
            mkdir(parent(&destination)?)?;
            rename(&source, &destination)?;
        }
        self.install_metadata(&destination, container, tx)?;
        self.trash_entry(entry, tx)
    }

    pub(super) fn detach_purge(&self, entry: Option<&Recycled>, op: &TxId) -> Result<Vec<PathBuf>> {
        if let Some(entry) = entry {
            return Ok(vec![self.trash_entry(entry, op)?]);
        }
        let mut cleanup = BTreeSet::new();
        for (disk, path) in self.recycle_paths(false)? {
            cleanup.insert(self.trash_entry(&Self::read_recycled(&disk, &path)?, op)?);
        }
        Ok(cleanup.into_iter().collect())
    }

    fn trash_entry(&self, entry: &Recycled, op: &TxId) -> Result<PathBuf> {
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
