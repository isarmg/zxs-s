mod rebuild;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
};

use rayon::prelude::*;

use super::{
    Change, ContainerId, Detail, File, FileName, FolderPath, Layout, NewContainer, Ordering,
    Pending, Result, State, Store, Version, invalid, new_tx_id, uploads::hash_file,
};

impl State {
    pub fn prune(&self, keep: usize, id: Option<ContainerId>) -> Result<usize> {
        if keep == 0 {
            return Err(invalid("keep-last must be at least 1"));
        }
        let paths = match id {
            Some(id) => vec![self.store.paths.container(&self.container(&id)?)],
            None => self.store.live_tree()?.containers,
        };
        let mut removed = 0;
        for path in paths {
            let detail = self.store.snapshot(&path)?;
            let to_remove = detail.versions.len().saturating_sub(keep);
            for chunk in detail.versions[..to_remove].chunks(4096) {
                let versions = chunk.iter().map(|v| v.number).collect();
                let pending = Pending {
                    tx_id: new_tx_id()?,
                    change: Change::Prune {
                        container: detail.container.clone(),
                        versions,
                    },
                };
                self.serialize_publication(|| self.transact(&pending, &mut []))?;
            }
            removed += to_remove;
        }
        Ok(removed)
    }

    pub fn import(&self, source: &Path, folder: &FolderPath) -> Result<usize> {
        self.store.paths.data_disk(folder)?;
        let mut items = Import::default();
        collect(source, folder, &mut items)?;
        for folder in items.folders {
            if !self.db.has_folder(&folder)? {
                self.edit_folder(Change::CreateFolder { folder })?;
            }
        }
        items
            .files
            .par_iter()
            .try_for_each(|(path, params)| self.import_payload(&File::open(path)?, params))?;
        Ok(items.files.len())
    }
}

pub fn verify(store: &Store) -> Result<usize> {
    let live = store.live_tree()?;
    let count = AtomicUsize::new(0);
    let damaged = AtomicUsize::new(0);
    live.containers.par_iter().for_each(|path| {
        let result = store
            .snapshot(path)
            .and_then(|detail| verify_container(path, &detail, &count));
        if let Err(error) = result {
            damaged.fetch_add(1, Ordering::Relaxed);
            tracing::error!(path = %path.display(), %error, "container verification failed");
        }
    });
    if damaged.into_inner() != 0 {
        return Err(invalid("live storage includes damaged containers"));
    }
    Ok(count.into_inner())
}

fn verify_container(path: &Path, detail: &Detail, count: &AtomicUsize) -> Result<()> {
    for v in &detail.versions {
        let p = Layout::payload_file(&Layout::version(path, v.number), &v.name_at_commit);
        verify_payload(&p, v)?;
        count.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

fn verify_payload(path: &Path, version: &Version) -> Result<()> {
    let (size, hash) = hash_file(File::open(path)?, io::sink())?;
    if size != version.size || hash.finalize().to_hex().as_str() != version.blake3.as_str() {
        return Err(invalid(format!(
            "payload verification failed: {}",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Default)]
struct Import {
    folders: Vec<FolderPath>,
    files: Vec<(PathBuf, NewContainer)>,
}

fn collect(dir: &Path, folder: &FolderPath, items: &mut Import) -> Result<()> {
    items.folders.push(folder.clone());
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("import path is not UTF-8"))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect(&entry.path(), &folder.child(&name)?, items)?;
        } else if kind.is_file() {
            items.files.push((
                entry.path(),
                NewContainer {
                    folder: folder.clone(),
                    name: FileName::parse(name)?,
                    tags: Vec::new(),
                },
            ));
        } else {
            return Err(invalid(format!(
                "unsupported import entry {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}
