use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use crate::{
    domain::{BUFFER, Blake3Hash, Error, Result, TxId},
    store::digest,
};

use super::{Layout, parent};

struct Scratch(Option<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(path) = &self.0
            && let Err(error) = fs::remove_file(path)
            && error.kind() != ErrorKind::NotFound
        {
            eprintln!("could not remove {}: {error}", path.display());
        }
    }
}

pub fn publish_staged_file(
    source: &Path,
    target: &Path,
    id: &TxId,
    hash: &Blake3Hash,
) -> Result<()> {
    match rename_new(source, target) {
        Ok(()) => return sync_parent(target),
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
        Err(error) => return Err(error.into()),
    }
    let name = Layout::checkout_scratch(target, id);
    let file = fs::File::create_new(&name)?;
    let mut scratch = Scratch(Some(name.clone()));
    let (_, actual) = digest::copy(fs::File::open(source)?, &file, BUFFER)?;
    if &actual != hash {
        return Err(Error::LocalConflict("检出暂存内容已改变".into()));
    }
    file.sync_all()?;
    drop(file);
    rename_new(&name, target)?;
    scratch.0 = None;
    sync_parent(target)
}

pub fn move_new(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(parent(destination)?)?;
    rename_new(source, destination)?;
    sync_parent(destination)?;
    sync_parent(source)
}

#[cfg(unix)]
pub(crate) fn rename_new(source: &Path, target: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, source, CWD, target, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(windows)]
fn rename_new(source: &Path, target: &Path) -> io::Result<()> {
    fn windows_path(path: &Path) -> io::Result<&str> {
        path.to_str()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid checkout path"))
    }
    let source_parent = source.parent().ok_or(ErrorKind::InvalidInput)?;
    let target_parent = target.parent().ok_or(ErrorKind::InvalidInput)?;
    if !super::checkout::same_volume(source_parent, target_parent).map_err(io::Error::other)? {
        return Err(ErrorKind::CrossesDevices.into());
    }
    winsafe::MoveFile(windows_path(source)?, windows_path(target)?).map_err(|code| {
        let raw: u32 = code.into();
        io::Error::from_raw_os_error(i32::from_ne_bytes(raw.to_ne_bytes()))
    })
}

pub fn sync_parent(path: &Path) -> Result<()> {
    let directory = parent(path)?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}
