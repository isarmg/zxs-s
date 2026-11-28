mod metadata;
mod moving;
mod recycling;
#[cfg(test)]
mod tests;
mod tree;

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    Layout, Store,
    fs_ops::{
        atomic_bytes_limited, entries, mkdir, parent, read_json, read_json_limited, remove_dir,
        remove_file, rename, sync, sync_parent, sync_rename,
    },
};
use crate::domain::{
    Change, Commit, Container, ContainerId, ContainerMeta, Error, FileName, FolderPath,
    METADATA_LIMIT, Pending, RecycleId, Recycled, Result, TxId, Version, VersionNum, invalid,
};

// A patch or restore can contain two metadata records plus their paths and journal fields.
const JOURNAL_LIMIT: usize = 2 * METADATA_LIMIT + 4 * 1024;

#[derive(Serialize, Deserialize)]
struct RecycleManifest {
    recycle_id: RecycleId,
    recycled_at: u64,
    original_name: FileName,
}

impl Store {
    pub fn start(&self, pending: &Pending) -> Result<()> {
        let path = self.journal(pending);
        if path.try_exists()? {
            return Err(Error::Recovery);
        }
        atomic_bytes_limited(&path, &serde_json::to_vec(pending)?, JOURNAL_LIMIT)
    }

    pub fn journal(&self, pending: &Pending) -> PathBuf {
        self.paths.pending(&pending.tx_id)
    }

    pub fn end(&self, pending: &Pending) -> Result<()> {
        remove_file(&self.journal(pending))
    }

    pub fn pending_changes(&self) -> Result<Vec<Pending>> {
        let paths = match entries(&self.paths.inflight()) {
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            result => result?,
        };
        let mut pending = Vec::new();
        for path in paths {
            match path.extension().and_then(OsStr::to_str) {
                Some("tmp") => continue,
                Some("items") if fs::symlink_metadata(&path)?.is_dir() => continue,
                Some("json") => {}
                _ => return Err(invalid(format!("unexpected journal {}", path.display()))),
            }
            let entry: Pending = read_json_limited(&path, JOURNAL_LIMIT)?;
            if self.journal(&entry) != path {
                return Err(invalid(format!("invalid journal {}", path.display())));
            }
            pending.push(entry);
        }
        Ok(pending)
    }

    pub fn clean_stages(&self) -> Result<()> {
        use rayon::prelude::*;
        self.paths.spools().par_iter().try_for_each(|root| {
            for path in entries(root)? {
                remove_dir(&path)?;
            }
            Ok::<(), Error>(())
        })?;
        for path in entries(&self.paths.inflight())? {
            match path.extension().and_then(OsStr::to_str) {
                Some("items") => remove_dir(&path)?,
                Some("tmp") => remove_file(&path)?,
                _ => {}
            }
        }
        Ok(())
    }

    pub fn apply(&self, pending: &Pending, archives: Option<&[Recycled]>) -> Result<Vec<PathBuf>> {
        let tx = &pending.tx_id;
        let mut cleanup = Vec::new();
        match &pending.change {
            Change::Publish { commit } => self.publish(commit, tx)?,
            Change::Patch {
                previous,
                container,
                staged,
            } => cleanup.extend(self.patch(previous, container, tx, *staged)?),
            Change::Recycle { source, entry } => {
                let source = source.child(entry.container.name.as_str())?;
                cleanup.extend(self.apply_archives(
                    &source,
                    tx,
                    std::slice::from_ref(entry),
                    archives.is_none(),
                )?);
            }
            Change::Restore { container, entry } => {
                cleanup.extend(self.install_restore(container, entry, tx)?);
            }
            Change::RecycleTree { folder } => {
                let detached = if let Some(archives) = archives {
                    self.apply_archives(folder, tx, archives, false)?
                } else {
                    let archives = self.recover_archives(pending)?;
                    self.apply_archives(folder, tx, &archives, true)?
                };
                cleanup.extend(detached);
            }
            Change::Purge { entry } => cleanup.push(self.trash_entry(entry, tx)?),
            Change::PurgeAll { disks } => cleanup = self.detach_all(disks, tx)?,
            Change::CreateFolder { folder } => {
                self.paths.data_disk(folder)?;
                mkdir(&self.paths.folder(folder))?;
            }
            Change::MoveFolder { from, to, staged } => {
                if *staged {
                    cleanup.extend(self.install_move(from, to, tx)?);
                } else {
                    self.move_folder(from, to)?;
                }
            }
            Change::DeleteFolder { folder } => {
                self.paths.data_disk(folder)?;
                remove_empty_folder(&self.paths.folder(folder))?;
            }
            Change::Prune {
                container,
                first_retained_version,
            } => self.apply_prune(container, *first_retained_version, tx)?,
        }
        Ok(cleanup)
    }

    fn apply_prune(&self, container: &Container, retained: VersionNum, tx: &TxId) -> Result<()> {
        self.paths.data_disk(&container.folder)?;
        let path = self.paths.container(container);
        let meta = require_container(&path, &container.id)?;
        let first = meta.first_retained_version;
        if first > retained
            || retained > meta.latest_version
            || meta.latest_version != container.latest_version
        {
            return Err(Error::Recovery);
        }
        for number in first.get()..retained.get() {
            let version = Layout::version(&path, VersionNum::new(number)?);
            if let Err(error) = fs::remove_dir_all(version)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(error.into());
            }
        }
        sync(&path)?;
        self.install_metadata(&path, container, tx)
    }

    fn publish(&self, commit: &Commit, tx: &TxId) -> Result<()> {
        let container = &commit.container;
        let disk = self.paths.data_disk(&container.folder)?;
        let version = &commit.version;
        let path = self.paths.container(container);
        let metadata = Layout::metadata(&path);
        if metadata.try_exists()? {
            require_container(&path, &container.id)?;
        }
        let destination = Layout::version(&path, version.number);
        if destination.try_exists()? {
            let published: Version = read_json(&Layout::metadata(&destination))?;
            if published.number != version.number || published.blake3 != version.blake3 {
                return Err(Error::Recovery);
            }
        } else {
            mkdir(&path)?;
            let stage = self.paths.staging(&disk, tx);
            rename(&Layout::staged_version(&stage), &destination)?;
        }
        self.install_metadata(&path, container, tx)
    }

    fn patch(
        &self,
        previous: &Container,
        container: &Container,
        tx: &TxId,
        staged: bool,
    ) -> Result<Option<PathBuf>> {
        if staged {
            let detached = self.install_move(
                &previous.folder.child(previous.name.as_str())?,
                &container.folder.child(container.name.as_str())?,
                tx,
            )?;
            self.install_metadata(&self.paths.container(container), container, tx)?;
            return Ok(detached);
        }
        self.paths.data_disk(&container.folder)?;
        let source = self.paths.container(previous);
        let target = self.paths.container(container);
        if source == target || target.try_exists()? {
            require_container(&target, &container.id)?;
            if source != target {
                if source.try_exists()? {
                    return Err(Error::Recovery);
                }
                sync_rename(&source, &target)?;
            }
        } else {
            require_container(&source, &container.id)?;
            mkdir(parent(&target)?)?;
            rename(&source, &target)?;
        }
        self.install_metadata(&target, container, tx)?;
        Ok(None)
    }

    fn move_folder(&self, from: &FolderPath, to: &FolderPath) -> Result<()> {
        self.paths.data_disk(to)?;
        let source = self.paths.folder(from);
        let destination = self.paths.folder(to);
        if source == destination {
            return require_folder(&source);
        }
        if destination.try_exists()? {
            if source.try_exists()? {
                return Err(Error::Recovery);
            }
            require_folder(&destination)?;
            return sync_rename(&source, &destination);
        }
        require_folder(&source)?;
        mkdir(parent(&destination)?)?;
        rename(&source, &destination)
    }
}

fn require_folder(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() && !Layout::metadata(path).try_exists()? {
        Ok(())
    } else {
        Err(Error::Recovery)
    }
}

fn remove_empty_folder(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn require_container(path: &Path, id: &ContainerId) -> Result<ContainerMeta> {
    let container: ContainerMeta = read_json(&Layout::metadata(path))?;
    if &container.id != id {
        return Err(Error::Recovery);
    }
    Ok(container)
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    copy_tree_contents(source, target)?;
    sync_parent(target)
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let original = entry.path();
        let copy = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree_contents(&original, &copy)?;
        } else if kind.is_file() {
            fs::copy(&original, &copy)?;
            sync(&copy)?;
        } else {
            return Err(invalid(format!(
                "unsupported entry in storage {}",
                original.display()
            )));
        }
    }
    sync(target)
}
