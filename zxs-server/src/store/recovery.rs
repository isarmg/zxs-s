#[cfg(test)]
mod tests;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{
    Layout, Store,
    fs_ops::{
        atomic_json, clean_stage, entries, mkdir, parent, read_json, remove_dir, remove_file,
        rename, sync, sync_parent,
    },
};
use crate::domain::{
    Change, Commit, Container, ContainerId, Error, OpId, Pending, Record, RecycleId, Recycled,
    Result, invalid,
};

impl Store {
    pub fn recover_changes(&self) -> Result<Vec<Pending>> {
        let mut pending = Vec::new();
        for path in entries(&self.paths.inflight())? {
            if is_temporary(&path) {
                remove_file(&path)?;
                continue;
            }
            let Some(id) = typed_name(&path, ContainerId::parse) else {
                continue;
            };
            let entry: Pending = read_json(&path)?;
            if entry.change.container_id() != &id {
                return Err(invalid(format!("invalid journal {}", path.display())));
            }
            pending.push(entry);
        }
        Ok(pending)
    }

    pub fn clean_stages(&self) -> Result<()> {
        for path in entries(&self.paths.active_stages())? {
            if is_temporary(&path) {
                remove_file(&path)?;
                continue;
            }
            let Some(op) = typed_name(&path, OpId::parse) else {
                continue;
            };
            let id: ContainerId = read_json(&path)?;
            clean_stage(&self.paths.staging(&id, &op), &path)?;
        }
        for path in entries(&self.paths.incoming())? {
            remove_dir(&path)?;
        }
        Ok(())
    }

    pub fn apply(&self, change: &Change) -> Result<()> {
        match change {
            Change::Publish { commit } => self.publish(commit),
            Change::Patch { container } => self.save_container(container),
            Change::Recycle { entry } => self.recycle(entry),
            Change::Restore {
                container,
                recycle_id,
            } => self.restore(container, recycle_id),
            Change::Prune {
                container_id,
                versions,
            } => {
                for version in versions {
                    remove_dir(&Layout::version(
                        &self.paths.container(container_id),
                        *version,
                    ))?;
                }
                Ok(())
            }
        }
    }

    fn save_container(&self, container: &Container) -> Result<()> {
        atomic_json(
            &Layout::container_meta(&self.paths.container(&container.id)),
            container,
        )
    }

    fn publish(&self, commit: &Commit) -> Result<()> {
        let container = &commit.container;
        let version = &commit.version;
        let path = self.paths.container(&container.id);
        let destination = Layout::version(&path, version.number);
        if !destination.try_exists()? {
            rename(
                &self.paths.staging(&container.id, &version.op_id),
                &destination,
            )?;
        }
        self.save_container(container)
    }

    fn recycle(&self, entry: &Recycled) -> Result<()> {
        let id = &entry.recycle.recycle_id;
        let final_path = self.paths.recycled_entry(id);
        if final_path.try_exists()? {
            return Ok(());
        }
        let incoming = self.paths.incoming_entry(id);
        mkdir(&incoming)?;
        atomic_json(&Layout::recycle_meta(&incoming), &entry.recycle)?;
        let destination = Layout::recycled_container(&incoming);
        move_container(&self.paths.container(&entry.container.id), &destination)?;
        for path in entries(&Layout::versions(&destination))? {
            if is_temporary(&path) {
                remove_dir(&path)?;
            }
        }
        mkdir(parent(&final_path)?)?;
        rename(&incoming, &final_path)
    }

    fn restore(&self, container: &Container, id: &RecycleId) -> Result<()> {
        let entry = self.paths.recycled_entry(id);
        let destination = self.paths.container(&container.id);
        if !Layout::container_meta(&destination).try_exists()? && destination.is_dir() {
            remove_empty_shell(&destination)?;
        }
        move_container(&Layout::recycled_container(&entry), &destination)?;
        self.save_container(container)?;
        remove_dir(&entry)
    }

    pub fn live_paths(&self) -> Result<Vec<PathBuf>> {
        sharded(&self.paths.containers(), |path| {
            typed_name(path, ContainerId::parse).is_some() && Layout::container_meta(path).is_file()
        })
    }
    pub fn recycle_paths(&self) -> Result<Vec<PathBuf>> {
        sharded(&self.paths.recycled(), |path| {
            typed_name(path, RecycleId::parse).is_some()
        })
    }
    pub fn operation_paths(&self) -> Result<Vec<PathBuf>> {
        sharded(&self.paths.operations(), |path| {
            path.extension().is_some_and(|ext| ext == "json")
        })
    }
    pub fn read_record(path: &Path) -> Result<Record> {
        read_json(path)
    }
    pub fn read_recycled(path: &Path) -> Result<Recycled> {
        Ok(Recycled {
            recycle: read_json(&Layout::recycle_meta(path))?,
            container: read_json(&Layout::container_meta(&Layout::recycled_container(path)))?,
        })
    }
}

fn sharded(root: &Path, filter: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for shard in entries(root)? {
        if shard.is_dir() {
            paths.extend(entries(&shard)?.into_iter().filter(|path| filter(path)));
        }
    }
    Ok(paths)
}

fn typed_name<T>(path: &Path, parse: impl FnOnce(String) -> Result<T>) -> Option<T> {
    parse(path.file_name()?.to_str()?.to_owned()).ok()
}

fn remove_empty_shell(path: &Path) -> Result<()> {
    let versions = Layout::versions(path);
    if versions.is_dir() && entries(&versions)?.iter().all(|p| is_temporary(p)) {
        remove_dir(&versions)?;
    }
    fs::remove_dir(path)?;
    sync_parent(path)
}

fn move_container(source: &Path, destination: &Path) -> Result<()> {
    if destination.try_exists()? {
        return remove_dir(source);
    }
    mkdir(parent(destination)?)?;
    match rename(source, destination) {
        Ok(()) => Ok(()),
        Err(Error::Io(err)) if err.kind() == io::ErrorKind::CrossesDevices => {
            let temp = Layout::transfer_dir(destination);
            remove_dir(&temp)?;
            copy_tree(source, &temp)?;
            rename(&temp, destination)?;
            remove_dir(source)
        }
        Err(error) => Err(error),
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let original = entry.path();
        if is_temporary(&original) {
            continue;
        }
        let copy = Layout::child(target, &entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&original, &copy)?;
        } else if kind.is_file() {
            fs::copy(&original, &copy)?;
            sync(&copy)?;
        } else {
            return Err(invalid(format!(
                "unsupported entry in storage: {}",
                original.display()
            )));
        }
    }
    sync(target)
}

fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.starts_with(".staging-"))
        || path.extension().is_some_and(|ext| ext == "tmp")
}
