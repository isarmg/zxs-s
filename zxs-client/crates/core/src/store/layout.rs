use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use crate::domain::{EntryKind, Error, LocalPath, METADATA_LIMIT, Result, Stamp, invalid};

use super::random_hex;

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
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(system.join(".lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok((Self { root, system }, lock)),
            Err(TryLockError::WouldBlock) => Err(Error::LocalConflict(
                "workspace is being used by another operation".into(),
            )),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub fn index(&self) -> PathBuf {
        self.system.join("workspace.sqlite")
    }

    pub fn local(&self, input: &Path) -> Result<LocalPath> {
        let path = if input.is_absolute() {
            input
                .strip_prefix(&self.root)
                .map_err(|_| invalid("local path is outside the workspace"))?
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
        LocalPath::parse(if segments.is_empty() {
            ".".into()
        } else {
            segments.join("/")
        })
    }

    pub fn path(&self, path: &LocalPath) -> Result<PathBuf> {
        checked_join(&self.root, path)
    }

    pub fn staged(&self) -> Result<TempDir> {
        TempDir::create(&self.system)
    }

    pub fn backup_path(&self, suffix: &str) -> Result<PathBuf> {
        let directory = self.system.join("backups");
        create_private(&directory)?;
        Ok(directory.join(format!("{}-{suffix}", random_hex::<8>()?)))
    }
}

pub fn kind(path: &Path) -> Result<Option<EntryKind>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(EntryKind::File)),
        Ok(metadata) if metadata.is_dir() => Ok(Some(EntryKind::Directory)),
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

pub fn checked_join(root: &Path, relative: &LocalPath) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    if relative.is_root() {
        return Ok(path);
    }
    for part in relative.as_str().split('/') {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid(format!(
                    "symbolic links are not allowed: {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
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
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(path) {
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
