use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use super::{Layout, RecycleManifest, Store, read_json};
use crate::domain::{
    ArchivedContainer, Container, ContainerMeta, DiskName, Error, FolderPath, RecycleId,
    RecycleInfo, Recycled, Result, invalid,
};
use crate::store::{DiskKind, fs_ops::container_meta};

pub struct Tree {
    pub folders: Vec<FolderPath>,
    pub containers: Vec<PathBuf>,
}

impl Store {
    pub fn check_subtree(&self, root: &Path, containers: &[Container]) -> Result<()> {
        let mut indexed: BTreeSet<_> = containers
            .iter()
            .map(|container| self.paths.container(container))
            .collect();
        Self::inspect_tree(root, |path| {
            if !indexed.remove(path) {
                return Err(Error::Conflict(format!(
                    "container missing from index: {}",
                    path.display()
                )));
            }
            Ok(())
        })?;
        if let Some(path) = indexed.into_iter().next() {
            return Err(Error::IncompleteHistory(path));
        }
        Ok(())
    }

    pub fn inspect_tree(root: &Path, mut visit: impl FnMut(&Path) -> Result<()>) -> Result<()> {
        if has_metadata(root)? {
            return visit(root);
        }
        walk(root, |path, container| {
            if container {
                visit(&path)?;
            }
            Ok(())
        })
    }

    pub fn live_tree(&self) -> Result<Tree> {
        use rayon::prelude::*;
        let disks = self
            .paths
            .data_disks
            .par_iter()
            .map(|disk| {
                let root = self
                    .paths
                    .disk_root(super::super::DiskKind::Data, &disk.name);
                let mut tree = Tree {
                    folders: Vec::new(),
                    containers: Vec::new(),
                };
                walk(&root, |path, container| {
                    if container {
                        tree.containers.push(path);
                    } else {
                        tree.folders.push(self.paths.folder_at(&path)?);
                    }
                    Ok(())
                })?;
                Ok(tree)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut tree = Tree {
            folders: vec![FolderPath::root()],
            containers: Vec::new(),
        };
        for mut disk in disks {
            tree.folders.append(&mut disk.folders);
            tree.containers.append(&mut disk.containers);
        }
        Ok(tree)
    }

    pub fn recycle_paths(&self, include_damaged: bool) -> Result<Vec<(DiskName, PathBuf)>> {
        use rayon::prelude::*;
        let batches = self
            .paths
            .recycle_disks
            .par_iter()
            .map(|disk| -> Result<_> {
                Ok(self
                    .recycle_batches(&disk.name, include_damaged)?
                    .into_iter()
                    .map(|path| (disk.name.clone(), path)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(batches.into_iter().flatten().collect())
    }

    pub(super) fn recycle_batches(
        &self,
        disk: &DiskName,
        include_damaged: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut items = Vec::new();
        for entry in fs::read_dir(self.paths.disk_root(DiskKind::Recycle, disk))? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir()
                && ((include_damaged && recycle_batch_name(&path)) || has_metadata(&path)?)
            {
                items.push(path);
            }
        }
        Ok(items)
    }

    pub(in crate::store) fn read_recycled(
        disk: &DiskName,
        path: &Path,
    ) -> Result<(Recycled, ContainerMeta)> {
        let manifest: RecycleManifest = read_json(&Layout::metadata(path))?;
        let directory = Layout::recycled_container(path, &manifest.original_name);
        let meta = container_meta(&directory)?;
        let entry = Recycled {
            recycle: RecycleInfo {
                recycle_id: manifest.recycle_id,
                recycled_at: manifest.recycled_at,
            },
            recycle_disk: disk.clone(),
            container: ArchivedContainer::at(manifest.original_name, meta.clone()),
        };
        Ok((entry, meta))
    }
}

fn recycle_batch_name(path: &Path) -> bool {
    let Some((date, id)) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('_'))
    else {
        return false;
    };
    date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
        && RecycleId::parse(id).is_ok()
}

fn walk(root: &Path, mut visit: impl FnMut(PathBuf, bool) -> Result<()>) -> Result<()> {
    let mut parents = vec![root.to_path_buf()];
    while let Some(path) = parents.pop() {
        let entry = path != root && has_metadata(&path)?;
        if !entry {
            for child in fs::read_dir(&path)? {
                let child = child?;
                let physical = child.path();
                if physical.file_name().is_some_and(|name| name == ".zxs") {
                    continue;
                }
                if !child.file_type()?.is_dir() {
                    return Err(invalid(format!(
                        "unexpected entry in storage {}",
                        physical.display()
                    )));
                }
                parents.push(physical);
            }
        }
        visit(path, entry)?;
    }
    Ok(())
}

fn has_metadata(directory: &Path) -> Result<bool> {
    match fs::symlink_metadata(Layout::metadata(directory)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
