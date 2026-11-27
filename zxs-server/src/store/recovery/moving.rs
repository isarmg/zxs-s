use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::{
    domain::{Error, FolderMove, FolderPath, Result, TxId},
    store::StageGuard,
};

use super::{Store, copy_tree, mkdir, parent, rename};

impl Store {
    pub fn prepare_move(
        &self,
        FolderMove { from, to }: &FolderMove,
        tx: &TxId,
        verify: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Option<StageGuard>> {
        let destination = self.paths.data_disk(to)?;
        if self.paths.data_disk(from)? == destination {
            return Ok(None);
        }
        let stage = self.stage(&destination, tx)?;
        let source = self.paths.folder(from);
        let target = self.paths.staged_tree(&destination, tx);
        match copy_tree(&source, &target).and_then(|()| verify(&target)) {
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                Err(Error::Conflict("source changed during copy".into()))
            }
            result => {
                result?;
                Ok(Some(stage))
            }
        }
    }

    pub(super) fn install_move(
        &self,
        from: &FolderPath,
        to: &FolderPath,
        tx: &TxId,
    ) -> Result<Option<PathBuf>> {
        let disk = self.paths.data_disk(to)?;
        let staged = self.paths.staged_tree(&disk, tx);
        let target = self.paths.folder(to);
        let tree = match fs::symlink_metadata(&staged) {
            Ok(tree) => tree,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if !fs::symlink_metadata(&target)?.is_dir() {
                    return Err(Error::Recovery);
                }
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if !tree.is_dir() || target.try_exists()? {
            return Err(Error::Recovery);
        }
        mkdir(parent(&target)?)?;
        // With the copy ready, retire the old path before the target becomes visible.
        let detached =
            self.detach_source(&self.paths.folder(from), &self.paths.data_disk(from)?, tx)?;
        rename(&staged, &target)?;
        Ok(Some(detached))
    }
}
