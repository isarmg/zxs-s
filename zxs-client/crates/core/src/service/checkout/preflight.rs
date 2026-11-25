use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use crate::{service::inspect, store::digest};

use super::{
    Checkout, CheckoutRequest, ContentHashes, EntryKind, Error, FileState, Install, Result, Track,
    Workspace, invalid, kind, status_one, zip_bundle,
};

impl Workspace {
    pub(super) fn checkout_backup(
        &self,
        path: &Path,
        prior: Option<&Track>,
        request: &CheckoutRequest,
    ) -> Result<Option<PathBuf>> {
        let Some(track) = prior else { return Ok(None) };
        if track.container_id != request.id {
            return Err(Error::LocalConflict(format!(
                "本地路径已追踪到 {} 上的其他文件容器",
                track.endpoint
            )));
        }
        match status_one(&self.layout, track)?.state {
            FileState::Missing => Ok(None),
            FileState::Clean if inspect::same_contents(&self.layout, track)? => Ok(None),
            _ => match request.choice {
                Checkout::IfClean => Err(Error::CheckoutConflict),
                Checkout::Backup => self.back_up(path).map(Some),
            },
        }
    }

    pub(super) fn back_up(&self, path: &Path) -> Result<PathBuf> {
        let destination = match kind(path)? {
            Some(EntryKind::Directory) => {
                let backup = self.layout.backup_path("bundle.zip")?;
                let result = zip_bundle::collect_backup(path)
                    .and_then(|items| zip_bundle::pack(path, &items, &backup));
                if result.is_err() && backup.is_file() {
                    let _ = fs::remove_file(&backup);
                }
                result?;
                backup
            }
            Some(EntryKind::File) => {
                let backup = self.layout.backup_path("file")?;
                fs::copy(path, &backup)?;
                File::open(&backup)?.sync_all()?;
                backup
            }
            None => return Err(invalid("local path disappeared during backup")),
        };
        Ok(destination)
    }
}
pub(super) fn verify_replacement(install: &Install) -> Result<()> {
    let Some(entry) = kind(&install.path)? else {
        return Ok(());
    };
    if entry == EntryKind::File {
        let current = digest::file(&install.path)?;
        if let Some(backup) = &install.backup {
            if digest::file(backup)? == current {
                return Ok(());
            }
        } else if let Some(Track {
            hashes: ContentHashes::File(previous),
            ..
        }) = &install.previous
            && previous == &current
        {
            return Ok(());
        }
    }
    if install.previous.is_none() && install.backup.is_none() {
        return Err(Error::LocalConflict(format!(
            "本地目标已存在 {}",
            install.path.display()
        )));
    }
    Err(Error::CheckoutConflict)
}
