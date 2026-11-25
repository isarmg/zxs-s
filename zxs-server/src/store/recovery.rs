mod metadata;
mod moving;
mod recycling;
#[cfg(test)]
mod tests;
mod tree;

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use super::{
    Layout, Store,
    fs_ops::{
        entries, mkdir, parent, read_json, remove_dir, remove_file, rename, sync, sync_parent,
    },
};
use crate::domain::{
    Change, Commit, Container, ContainerId, ContainerMeta, Error, FileName, FolderPath, Pending,
    RecycleId, Result, TxId, Version, invalid,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecycleManifest {
    recycle_id: RecycleId,
    recycled_at: u64,
    original_folder: FolderPath,
    original_name: FileName,
}

impl Store {
    pub fn recover_changes(&self) -> Result<Vec<Pending>> {
        let mut pending = Vec::new();
        for path in entries(&self.paths.inflight())? {
            if path
                .extension()
                .is_some_and(|extension| extension == "items")
                && fs::symlink_metadata(&path)?.is_dir()
            {
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "tmp") {
                remove_file(&path)?;
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "json") {
                return Err(invalid(format!("unexpected journal {}", path.display())));
            }
            let entry: Pending = read_json(&path)?;
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
            if path
                .extension()
                .is_some_and(|extension| extension == "items")
            {
                remove_dir(&path)?;
            }
        }
        Ok(())
    }

    pub fn apply(&self, pending: &Pending) -> Result<()> {
        match &pending.change {
            Change::Publish { commit } => self.publish(commit, &pending.tx_id),
            Change::Patch {
                previous,
                container,
                staged,
            } => self.patch(previous, container, &pending.tx_id, *staged),
            Change::Recycle { entry, staged } => self.apply_recycle(entry, &pending.tx_id, *staged),
            Change::Restore {
                container,
                entry,
                staged,
            } => self.install_restore(container, entry, pending, *staged),
            Change::RecycleTree { folder } => self.apply_recycle_tree(folder, &pending.tx_id),
            Change::Purge { entry } => self.detach_purge(entry.as_deref(), &pending.tx_id),
            Change::CreateFolder { folder } => mkdir(&self.paths.folder(folder)),
            Change::MoveFolder { from, to, staged } => {
                if *staged {
                    self.install_move(from, to, &pending.tx_id)
                } else {
                    self.move_folder(from, to)
                }
            }
            Change::DeleteFolder { folder } => remove_empty_folder(&self.paths.folder(folder)),
            Change::Prune {
                container,
                versions,
            } => {
                let path = self.paths.container(container);
                require_container(&path, &container.id)?;
                for version in versions {
                    remove_dir(&Layout::version(&path, *version))?;
                }
                Ok(())
            }
        }
    }

    fn publish(&self, commit: &Commit, tx: &TxId) -> Result<()> {
        let container = &commit.container;
        let version = &commit.version;
        let path = self.paths.container(container);
        let metadata = Layout::container_meta(&path);
        if metadata.try_exists()? {
            require_container(&path, &container.id)?;
        }
        let destination = Layout::version(&path, version.number);
        if destination.try_exists()? {
            let published: Version = read_json(&Layout::version_meta(&destination))?;
            if published.number != version.number || published.blake3 != version.blake3 {
                return Err(Error::Recovery);
            }
        } else {
            mkdir(&path)?;
            let stage = self.paths.staging(&container.folder.disk()?, tx);
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
    ) -> Result<()> {
        if staged {
            self.install_move(
                &previous.folder.child(previous.name.as_str())?,
                &container.folder.child(container.name.as_str())?,
                tx,
            )?;
            return self.install_metadata(&self.paths.container(container), container, tx);
        }
        let source = self.paths.container(previous);
        let target = self.paths.container(container);
        if source == target {
            require_container(&target, &container.id)?;
            return self.install_metadata(&target, container, tx);
        }
        if target.try_exists()? {
            require_container(&target, &container.id)?;
            if source.try_exists()? {
                return Err(Error::Recovery);
            }
        } else {
            require_container(&source, &container.id)?;
            mkdir(parent(&target)?)?;
            rename(&source, &target)?;
        }
        self.install_metadata(&target, container, tx)
    }

    fn move_folder(&self, from: &FolderPath, to: &FolderPath) -> Result<()> {
        let source = self.paths.folder(from);
        let destination = self.paths.folder(to);
        if source == destination {
            return require_folder(&source);
        }
        if destination.try_exists()? {
            if source.try_exists()? {
                return Err(Error::Recovery);
            }
            return require_folder(&destination);
        }
        require_folder(&source)?;
        mkdir(parent(&destination)?)?;
        rename(&source, &destination)
    }
}

fn require_folder(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() && !Layout::container_meta(path).try_exists()? {
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

fn require_container(path: &Path, id: &ContainerId) -> Result<()> {
    let container: ContainerMeta = read_json(&Layout::container_meta(path))?;
    if &container.id != id {
        return Err(Error::Recovery);
    }
    Ok(())
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
