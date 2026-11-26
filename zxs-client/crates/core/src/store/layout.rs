use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use crate::domain::{EntryKind, Error, LocalPath, METADATA_LIMIT, Result, Stamp, TxId, invalid};

use super::random_hex;

mod case_move;
mod checkout;
mod publication;
mod spelling;
#[cfg(unix)]
pub(crate) use publication::rename_new;
pub use publication::{move_new, publish_staged_file, sync_parent};
use spelling::join_spelling;
pub use spelling::{check_unique_name, checked_join};

pub const WORKSPACE_DIR: &str = ".zxs";
pub const WORKSPACE_MOVE_MARKER: &str = ".workspace-move";
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
        ensure_stationary(&system)?;
        let lock = lock_file(&system.join(".lock"))?;
        ensure_stationary(&system)?;
        let layout = Self { root, system };
        layout.recover_case_move()?;
        Ok((layout, lock))
    }

    pub fn index(&self) -> PathBuf {
        Self::index_at(&self.root)
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
        if segments
            .first()
            .is_some_and(|name| name.eq_ignore_ascii_case(WORKSPACE_DIR))
        {
            return Err(invalid(".zxs is reserved for workspace metadata"));
        }
        if segments.is_empty() {
            return LocalPath::parse(".");
        }
        let supplied = segments.join("/");
        let local = LocalPath::parse(supplied.as_str())?;
        if supplied != local.as_str() {
            join_spelling(&self.root, &local, &supplied)?;
        }
        Ok(local)
    }

    pub fn path(&self, path: &LocalPath) -> Result<PathBuf> {
        checked_join(&self.root, path)
    }

    pub fn staged(&self) -> Result<TempDir> {
        TempDir::create(&self.system)
    }

    pub fn checkout_scratch(path: &Path, id: &TxId) -> PathBuf {
        path.with_file_name(format!(".zxs-checkout-{}", id.as_str()))
    }
}

fn ensure_stationary(system: &Path) -> Result<()> {
    match fs::symlink_metadata(system.join(WORKSPACE_MOVE_MARKER)) {
        Ok(_) => Err(Error::LocalConflict("工作区正在迁移，请稍后再试".into())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
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
    if let Ok(relative) = strip_root(input, root) {
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

#[cfg(not(windows))]
fn strip_root<'a>(input: &'a Path, root: &Path) -> std::result::Result<&'a Path, ()> {
    input.strip_prefix(root).map_err(|_| ())
}

#[cfg(windows)]
fn strip_root<'a>(input: &'a Path, root: &Path) -> std::result::Result<&'a Path, ()> {
    use std::path::Prefix::{Disk, UNC, VerbatimDisk, VerbatimUNC};
    let mut parts = input.components();
    let mut roots = root.components();
    let (Some(Component::Prefix(input)), Some(Component::Prefix(root))) =
        (parts.next(), roots.next())
    else {
        return Err(());
    };
    let same_drive = match (input.kind(), root.kind()) {
        (Disk(a) | VerbatimDisk(a), Disk(b) | VerbatimDisk(b)) => a.eq_ignore_ascii_case(&b),
        (UNC(a, b) | VerbatimUNC(a, b), UNC(c, d) | VerbatimUNC(c, d)) => {
            a.eq_ignore_ascii_case(c) && b.eq_ignore_ascii_case(d)
        }
        (a, b) => a == b,
    };
    if !same_drive
        || !roots.all(|root| {
            parts
                .next()
                .is_some_and(|part| part.as_os_str().eq_ignore_ascii_case(root.as_os_str()))
        })
    {
        return Err(());
    }
    Ok(parts.as_path())
}

pub fn kind(path: &Path) -> Result<Option<EntryKind>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(EntryKind::File)),
        Ok(metadata) if metadata.is_dir() => Ok(Some(EntryKind::Directory)),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::SymbolicLink(path.into())),
        Ok(_) => Err(invalid(format!(
            "unsupported local entry {}",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
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
                    if error.kind() == io::ErrorKind::AlreadyExists
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
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("could not remove {}: {error}", self.path.display());
        }
    }
}
