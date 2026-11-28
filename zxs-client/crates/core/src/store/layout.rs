use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, ErrorKind, Read},
    path::{Component, Path, PathBuf},
};

use crate::domain::{EntryKind, Error, LocalPath, METADATA_LIMIT, Result, Stamp, TxId, invalid};

use super::random_hex;

mod case_move;
mod checkout;
mod publication;
mod spelling;
pub use publication::{move_new, publish_staged_file, sync_parent};
use spelling::join_spelling;
pub use spelling::{check_unique_name, checked_join};

pub const WORKSPACE_DIR: &str = ".zxs";
pub const IGNORE_FILE: &str = ".zxsignore";

pub struct Layout {
    pub root: PathBuf,
    pub system: PathBuf,
}

impl Layout {
    pub fn open(root: &Path) -> Result<(Self, File)> {
        let root = root.canonicalize()?;
        let system = root.join(WORKSPACE_DIR);
        create_private(&system)?;
        let lock = lock_file(&system.join(".lock"))?;
        let layout = Self { root, system };
        layout.recover_case_move()?;
        Ok((layout, lock))
    }

    pub fn index_at(root: &Path) -> PathBuf {
        root.join(WORKSPACE_DIR).join("workspace.sqlite")
    }

    pub fn local(&self, input: &Path) -> Result<LocalPath> {
        let path = if input.is_absolute() {
            if input.components().any(|part| part == Component::ParentDir) {
                return Err(invalid("local path must stay inside the workspace"));
            }
            workspace_relative(input, &self.root)
                .ok_or_else(|| invalid("local path is outside the workspace"))?
        } else {
            input
        };
        let segments = path
            .components()
            .filter(|part| !matches!(part, Component::CurDir))
            .map(|part| match part {
                Component::Normal(part) => part
                    .to_str()
                    .ok_or_else(|| invalid("local path is not UTF-8")),
                _ => Err(invalid("local path must stay inside the workspace")),
            })
            .collect::<Result<Vec<_>>>()?;
        let supplied = segments.join("/");
        let local = LocalPath::parse(if supplied.is_empty() { "." } else { &supplied })?;
        if supplied != local.as_str() {
            join_spelling(&self.root, &local, &supplied, 0)?;
        }
        Ok(local)
    }

    pub fn path(&self, path: &LocalPath) -> Result<PathBuf> {
        checked_join(&self.root, path)
    }

    pub fn checkout_path(&self, path: &LocalPath, check_leaf: bool) -> Result<PathBuf> {
        let folder_depth = path.depth() - usize::from(!check_leaf);
        join_spelling(&self.root, path, path.as_str(), folder_depth)
    }

    pub fn staged(&self) -> Result<TempDir> {
        TempDir::create(&self.system)
    }

    pub fn checkout_scratch(path: &Path, id: &TxId) -> PathBuf {
        path.with_file_name(format!(".zxs-checkout-{}", id.as_str()))
    }
}

pub fn lock_file(path: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(Error::LocalConflict(
            "workspace is being used by another operation".into(),
        )),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn workspace_relative<'a>(input: &'a Path, root: &Path) -> Option<&'a Path> {
    if let Ok(relative) = input.strip_prefix(root) {
        return Some(relative);
    }
    // Find the first spelling of the root, then leave its suffix for checked_join.
    let ancestors: Vec<_> = input.ancestors().collect();
    let alias = ancestors
        .into_iter()
        .rev()
        .find(|path| path.canonicalize().is_ok_and(|canonical| canonical == root))?;
    input.strip_prefix(alias).ok()
}

pub fn same_existing_path(left: &Path, right: &Path) -> bool {
    left.canonicalize()
        .is_ok_and(|resolved| right.canonicalize().is_ok_and(|actual| actual == resolved))
}

pub fn kind(path: &Path) -> Result<Option<EntryKind>> {
    Ok(entry_metadata(path)?.map(|(kind, _)| kind))
}

pub fn entry_metadata(path: &Path) -> Result<Option<(EntryKind, fs::Metadata)>> {
    match if_present(fs::symlink_metadata(path))? {
        Some(metadata) if metadata.is_file() => Ok(Some((EntryKind::File, metadata))),
        Some(metadata) if metadata.is_dir() => Ok(Some((EntryKind::Directory, metadata))),
        Some(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::SymbolicLink(path.into()))
        }
        Some(_) => Err(invalid(format!(
            "unsupported local entry {}",
            path.display()
        ))),
        None => Ok(None),
    }
}

pub fn if_present<T>(result: io::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| invalid(format!("no parent for {}", path.display())))
}

pub fn stamp(path: &Path) -> Result<Stamp> {
    Stamp::from_metadata(&fs::metadata(path)?)
}

pub fn read_small(path: &Path) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    File::open(path)?
        .take(METADATA_LIMIT as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() > METADATA_LIMIT {
        return Err(invalid(format!(
            "metadata exceeds 1 MiB: {}",
            path.display()
        )));
    }
    Ok(data)
}

pub fn create_private(path: &Path) -> Result<()> {
    match kind(path)? {
        Some(EntryKind::Directory) => Ok(()),
        Some(EntryKind::File) => Err(invalid(format!("directory required: {}", path.display()))),
        None => {
            #[cfg(unix)]
            let result = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(path)
            };
            #[cfg(not(unix))]
            let result = fs::create_dir(path);
            match result {
                Ok(()) => Ok(()),
                Err(error)
                    if error.kind() == ErrorKind::AlreadyExists
                        && kind(path)? == Some(EntryKind::Directory) =>
                {
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
    }
}

pub struct TempDir {
    path: PathBuf,
    cleanup: bool,
}

impl TempDir {
    pub fn create(parent: &Path) -> Result<Self> {
        let path = parent.join(format!(".zxs-stage-{}", random_hex::<16>()?));
        create_private(&path)?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }

    pub fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
    pub fn id(&self) -> Result<TxId> {
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(".zxs-stage-"))
            .ok_or_else(|| invalid("invalid staging directory"))?;
        TxId::parse(name)
    }
    pub fn preserve(&mut self) {
        self.cleanup = false;
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.cleanup
            && let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != ErrorKind::NotFound
        {
            eprintln!("could not remove {}: {error}", self.path.display());
        }
    }
}
