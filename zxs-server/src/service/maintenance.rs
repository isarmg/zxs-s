mod rebuild;

pub(super) use rebuild::rebuild;

use std::{
    collections::HashSet,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use rayon::prelude::*;

use super::{
    Change, ContainerId, File, FileName, FolderPath, NewContainer, Pending, Result, State, Store,
    invalid, new_tx_id, publication::Prepared,
};

impl State {
    pub fn prune(&self, keep: NonZeroUsize, id: Option<ContainerId>) -> Result<usize> {
        let keep = keep.get();
        let paths = self.db.prune_candidates(keep, id.as_ref())?;
        if let Some(id) = id
            && paths.is_empty()
        {
            self.container(&id)?;
        }
        let mut removed = 0;
        for (folder, name) in paths {
            let path = self.store.paths.container_at(&folder, &name);
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
        for folder in items.leaf_folders {
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
    if !store.pending_changes()?.is_empty() {
        return Err(invalid(
            "run zxs rebuild-index to finish pending transactions before verify",
        ));
    }
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
    leaf_folders: Vec<FolderPath>,
    files: Vec<(PathBuf, NewContainer)>,
}

fn collect(dir: &Path, folder: &FolderPath, items: &mut Import) -> Result<()> {
    let mut has_subdirectory = false;
    let mut names = HashSet::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("import path is not UTF-8"))?;
        let name = FileName::parse(name)?;
        if !names.insert(name.key()) {
            return Err(invalid(format!(
                "import names collide after normalization: {}",
                entry.path().display()
            )));
        }
        let target = folder.child(name.as_str())?;
        if kind.is_dir() {
            has_subdirectory = true;
            collect(&entry.path(), &target, items)?;
        } else if kind.is_file() {
            items.files.push((
                entry.path(),
                NewContainer {
                    folder: folder.clone(),
                    name,
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
    if !has_subdirectory {
        items.leaf_folders.push(folder.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };

    use crate::{domain::Error, test_support::TestDir};

    use super::{Change, FolderPath, Import, Pending, Result, collect, fs, new_tx_id, verify};

    #[test]
    fn verify_keeps_unfinished_storage_and_reports_pending_transactions() -> Result<()> {
        let root = TestDir::create()?;
        let store = root.store()?;
        let temporary = store.paths.inflight().join("unpublished.tmp");
        let plan = store.paths.inflight().join("unpublished.items");
        fs::write(&temporary, b"partial")?;
        fs::create_dir(&plan)?;
        assert_eq!(verify(&store)?, 0);

        let pending = Pending {
            tx_id: new_tx_id()?,
            change: Change::CreateFolder {
                folder: FolderPath::parse("/disk1/pending")?,
            },
        };
        store.start(&pending)?;
        assert!(
            matches!(verify(&store), Err(Error::Invalid(message)) if message.contains("rebuild-index"))
        );
        assert!(temporary.is_file() && plan.is_dir() && store.journal(&pending).is_file());
        Ok(())
    }

    #[test]
    fn import_rejects_sibling_directories_with_equivalent_names() -> Result<()> {
        let source = TestDir::create()?;
        fs::create_dir(source.0.join("Café"))?;
        fs::create_dir(source.0.join("Cafe\u{301}"))?;
        let folder = FolderPath::parse("/disk1/imported")?;
        assert!(matches!(
            collect(&source.0, &folder, &mut Import::default()),
            Err(Error::Invalid(message)) if message.contains("import names collide")
        ));
        Ok(())
    }

    #[test]
    fn import_skips_symlinks_with_non_utf8_names() -> Result<()> {
        let source = TestDir::create()?;
        fs::write(source.0.join("file.txt"), b"content")?;
        symlink("file.txt", source.0.join(OsStr::from_bytes(b"link-\xff")))?;
        let mut items = Import::default();
        collect(
            &source.0,
            &FolderPath::parse("/disk1/imported")?,
            &mut items,
        )?;
        assert_eq!(items.files.len(), 1);
        assert_eq!(items.files[0].1.name.as_str(), "file.txt");
        Ok(())
    }

    #[test]
    fn import_precreates_only_leaf_folders() -> Result<()> {
        let source = TestDir::create()?;
        fs::create_dir_all(source.0.join("docs/notes"))?;
        fs::create_dir_all(source.0.join("empty/leaf"))?;
        fs::write(source.0.join("docs/notes/file.txt"), b"content")?;
        let folder = FolderPath::parse("/disk1/imported")?;
        let mut items = Import::default();
        collect(&source.0, &folder, &mut items)?;
        items.leaf_folders.sort_unstable_by_key(FolderPath::key);
        assert_eq!(
            items.leaf_folders,
            [
                FolderPath::parse("/disk1/imported/docs/notes")?,
                FolderPath::parse("/disk1/imported/empty/leaf")?,
            ]
        );
        assert_eq!(items.files.len(), 1);
        Ok(())
    }

    #[test]
    fn import_rejects_overlong_destination_before_copying() -> Result<()> {
        let source = TestDir::create()?;
        fs::write(source.0.join("a-long-name.txt"), b"content")?;
        let component = "a".repeat(250);
        let folder = FolderPath::parse(format!(
            "/disk1/{component}/{component}/{component}/{component}"
        ))?;
        assert!(matches!(
            collect(&source.0, &folder, &mut Import::default()),
            Err(Error::Invalid(message)) if message == "invalid folder path"
        ));
        Ok(())
    }
}
