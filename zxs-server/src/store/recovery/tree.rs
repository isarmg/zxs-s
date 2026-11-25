use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{Layout, RecycleManifest, Store, entries, read_json};
use crate::domain::{
    Container, ContainerMeta, DiskName, Error, FolderPath, RecycleInfo, Recycled, Result, invalid,
};

pub struct Tree {
    pub folders: Vec<FolderPath>,
    pub containers: Vec<PathBuf>,
}

impl Store {
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
                walk(&root, looks_like_container, |path, container| {
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

    pub fn recycle_paths(&self) -> Result<Vec<(DiskName, PathBuf)>> {
        use rayon::prelude::*;
        let batches = self
            .paths
            .recycle_disks
            .par_iter()
            .map(|disk| {
                let root = self
                    .paths
                    .disk_root(super::super::DiskKind::Recycle, &disk.name);
                let mut items = Vec::new();
                for path in entries(&root)? {
                    if fs::symlink_metadata(&path)?.is_dir()
                        && has_marker(&Layout::recycle_meta(&path))?
                    {
                        items.push((disk.name.clone(), path));
                    }
                }
                Ok(items)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(batches.into_iter().flatten().collect())
    }

    pub fn read_recycled(disk: &DiskName, path: &Path) -> Result<Recycled> {
        let manifest: RecycleManifest = read_json(&Layout::recycle_meta(path))?;
        let directory = Layout::recycled_container(path, &manifest.original_name);
        let meta: ContainerMeta = read_json(&Layout::container_meta(&directory))?;
        Ok(Recycled {
            recycle: RecycleInfo {
                recycle_id: manifest.recycle_id,
                recycled_at: manifest.recycled_at,
            },
            recycle_disk: disk.clone(),
            container: Container::at(manifest.original_folder, manifest.original_name, meta),
        })
    }
}

pub(super) fn walk(
    root: &Path,
    is_container: impl Fn(&Path) -> Result<bool>,
    mut visit: impl FnMut(PathBuf, bool) -> Result<()>,
) -> Result<()> {
    let mut parents = vec![root.to_path_buf()];
    while let Some(path) = parents.pop() {
        let entry = path != root && is_container(&path)?;
        if !entry {
            for child in entries(&path)? {
                if child.file_name().is_some_and(|name| name == ".zxs") {
                    continue;
                }
                if !fs::symlink_metadata(&child)?.is_dir() {
                    return Err(invalid(format!(
                        "unexpected entry in storage {}",
                        child.display()
                    )));
                }
                parents.push(child);
            }
        }
        visit(path, entry)?;
    }
    Ok(())
}

pub(super) fn looks_like_container(path: &Path) -> Result<bool> {
    if Layout::container_system(path).try_exists()? {
        return Ok(true);
    }
    for child in fs::read_dir(path)? {
        let child = child?;
        let name = child.file_name();
        let Some(digits) = name.to_str().and_then(|name| name.strip_prefix('v')) else {
            continue;
        };
        if digits.len() >= 6
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && child.file_type()?.is_dir()
            && has_marker(&Layout::version_meta(&child.path()))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_marker(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(Error::Recovery),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
