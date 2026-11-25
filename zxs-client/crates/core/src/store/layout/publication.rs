use std::{
    fs,
    io::ErrorKind,
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
    let name = Layout::checkout_scratch(target, id);
    let file = fs::File::create_new(&name)?;
    let mut scratch = Scratch(Some(name.clone()));
    drop(file);
    match fs::rename(source, &name) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {
            let file = fs::File::create(&name)?;
            let (_, actual) = digest::copy(fs::File::open(source)?, &file, BUFFER)?;
            if &actual != hash {
                return Err(Error::LocalConflict("检出暂存内容已改变".into()));
            }
            file.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    fs::rename(&name, target)?;
    scratch.0 = None;
    sync_parent(target)
}

pub fn sync_parent(path: &Path) -> Result<()> {
    let directory = parent(path)?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}
