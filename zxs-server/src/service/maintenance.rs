mod rebuild;

pub(super) use rebuild::rebuild;

use std::{
    fs,
    path::{Path, PathBuf},
};

use rayon::prelude::*;

use super::{
    Change, ContainerId, File, FileName, FolderPath, NewContainer, Pending, Result, State, Store,
    invalid, new_tx_id, publication::Prepared,
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
            if to_remove == 0 {
                continue;
            }
            let pending = Pending {
                tx_id: new_tx_id()?,
                change: Change::Prune {
                    container: detail.container,
                    first_retained_version: detail.versions[to_remove].number,
                },
            };
            let cleanup =
                self.serialize_publication(|| self.transact(&pending, &mut [], Prepared::None))?;
            Store::finish(cleanup);
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
    let archives = store.recycle_paths(true)?;
    let live = live.containers.par_iter().map(|path| {
        let result = store
            .paths
            .container_location(path)
            .and_then(|_| Store::verify_versions(path));
        verify_count(path, result)
    });
    let archives = archives
        .par_iter()
        .map(|(disk, path)| verify_count(path, store.verify_archive(disk, path)));
    let (count, damaged) = live.chain(archives).reduce(
        || (0, 0),
        |(count, damaged), (checked, errors)| (count + checked, damaged + errors),
    );
    if damaged != 0 {
        return Err(invalid("storage includes damaged containers"));
    }
    store.check_indexed_paths()?;
    Ok(count)
}

fn verify_count(path: &Path, result: Result<usize>) -> (usize, usize) {
    match result {
        Ok(count) => (count, 0),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "container verification failed");
            (0, 1)
        }
    }
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
