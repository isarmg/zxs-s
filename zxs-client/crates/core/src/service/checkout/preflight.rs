use std::path::{Path, PathBuf};

use crate::{
    domain::{FileState, Snapshot},
    service::status_one,
    store::layout::check_unique_name,
};

use super::{
    Checkout, CheckoutRequest, Container, DownloadVersion, EntryKind, Error, Install,
    InterruptedCheckout, LocalPath, Result, Revision, Track, Workspace, kind,
};

pub(super) struct Target<'a> {
    pub container: &'a Container,
    pub prior: Option<Track>,
    pub interrupted: Option<InterruptedCheckout>,
    pub bundle: bool,
}

pub(super) fn restoration_baseline(
    request: &CheckoutRequest,
    (container, version): (&Container, &DownloadVersion),
    prior: Option<&Track>,
    interrupted: Option<&InterruptedCheckout>,
) -> Result<Option<Revision>> {
    if let Some(entry) = interrupted {
        if !matches!(request.choice, Checkout::Backup)
            || request
                .version
                .is_some_and(|version| version != entry.version)
        {
            return Err(Error::CheckoutInterrupted);
        }
        let hash = entry.payload_hash.as_ref().ok_or_else(|| {
            Error::LocalConflict("中断检出记录缺少原件哈希，已保留本地文件与备份".into())
        })?;
        if hash != &version.blake3 {
            return Err(Error::LocalConflict(format!(
                "上次恢复的 v{} 内容已改变，已保留本地数据",
                entry.version
            )));
        }
        return Ok(entry.restored_against.clone());
    }
    Ok((version.number != container.latest_version)
        .then(|| prior.map_or_else(|| Revision::from(container), Track::baseline)))
}

impl Workspace {
    pub(super) fn prepare_install(
        &self,
        request: &CheckoutRequest,
        target: Target<'_>,
    ) -> Result<Install> {
        let local = self.checkout_destination(request, &target)?;
        let path = self.layout.path(&local)?;
        let source = target
            .prior
            .as_ref()
            .map(|track| self.layout.path(&track.path))
            .transpose()?;
        let confirmed = matches!(request.choice, Checkout::Backup);
        let permit_untracked =
            confirmed && request.version.is_none() && request.destination.is_some();
        let occupied = kind(&path)?;
        if occupied.is_some() {
            check_unique_name(&path)?;
        }
        if occupied.is_some()
            && source.as_deref() != Some(path.as_path())
            && target.interrupted.is_none()
            && !permit_untracked
            && !(target.bundle && occupied == Some(EntryKind::Directory))
        {
            return Err(occupied_path(&path));
        }
        self.check_local_target(&local, target.prior.as_ref().map(|track| &track.path))?;
        if !confirmed
            && let Some(track) = &target.prior
            && matches!(status_one(&self.layout, track)?.state, FileState::Modified)
        {
            return Err(Error::CheckoutConflict);
        }
        let mirrored = target.prior.as_ref().is_some_and(|track| track.mirrored)
            || local == self.destination(target.container)?;
        Ok(Install {
            path,
            local,
            source,
            mirrored,
            confirmed,
            accept_untracked: permit_untracked,
            previous: target.prior,
            kind: if target.bundle {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            interrupted: target.interrupted,
        })
    }

    fn checkout_destination(
        &self,
        request: &CheckoutRequest,
        target: &Target<'_>,
    ) -> Result<LocalPath> {
        let explicit = request
            .destination
            .as_deref()
            .map(|path| self.layout.local(path))
            .transpose()?;
        if let Some(entry) = &target.interrupted {
            if explicit.as_ref().is_some_and(|path| path != &entry.path) {
                return Err(Error::CheckoutInterrupted);
            }
            return Ok(entry.path.clone());
        }
        if let Some(path) = explicit {
            if let Some(track) = &target.prior
                && track.path != path
            {
                return Err(occupied_path(&self.layout.path(&track.path)?));
            }
            return Ok(path);
        }
        if let Some(track) = &target.prior
            && (!track.mirrored || matches!(track.snapshot, Snapshot::Bundle(_)) == target.bundle)
        {
            return Ok(track.path.clone());
        }
        self.destination(target.container)
    }
}

pub(super) fn occupied_path(path: &Path) -> Error {
    Error::LocalConflict(format!("本地目标已被占用 {}", path.display()))
}

impl Install {
    pub(super) fn prior_here(&self) -> Option<&Track> {
        self.previous
            .as_ref()
            .filter(|track| track.path == self.local)
    }

    pub(super) fn owns_target(&self) -> bool {
        self.prior_here().is_some() || self.interrupted.is_some() || self.accept_untracked
    }

    pub(super) fn source_elsewhere(&self) -> Option<&Path> {
        self.source.as_deref().filter(|source| *source != self.path)
    }

    pub(super) fn previous_backup(&self, workspace: &Workspace) -> Option<PathBuf> {
        self.interrupted
            .as_ref()?
            .backup
            .as_ref()
            .map(|path| workspace.layout.root.join(path))
            .filter(|path| path.exists())
    }
}
