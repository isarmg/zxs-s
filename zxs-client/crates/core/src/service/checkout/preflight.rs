use std::path::{Path, PathBuf};

use crate::store::layout::check_unique_name;

use super::{
    Checkout, CheckoutRequest, EntryKind, Error, Install, LocalPath, Result, Revision, Selection,
    Track, Workspace, kind, zip_bundle,
};
use crate::domain::Snapshot;

pub(super) fn restoration_baseline(
    request: &CheckoutRequest,
    selection: &Selection<'_>,
) -> Result<Option<Revision>> {
    if let Some(entry) = &selection.interrupted {
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
        if hash != &selection.version.blake3 {
            return Err(Error::LocalConflict(format!(
                "上次恢复的 v{} 内容已改变，已保留本地数据",
                entry.version
            )));
        }
        return Ok(entry.restored_against.clone());
    }
    Ok(
        (selection.version.number != selection.container.latest_version).then(|| {
            selection
                .prior
                .as_ref()
                .map_or_else(|| Revision::from(selection.container), Track::baseline)
        }),
    )
}

impl Workspace {
    pub(super) fn prepare_install(
        &self,
        request: &CheckoutRequest,
        target: Selection<'_>,
        bundle: bool,
    ) -> Result<Install> {
        let local = self.checkout_destination(request, &target)?;
        let prior = target.prior.as_ref();
        if bundle {
            zip_bundle::require_bundle_suffix(local.name())?;
        } else if target.interrupted.is_none()
            && prior.is_some_and(|track| matches!(track.snapshot, Snapshot::Bundle(_)))
        {
            zip_bundle::require_bundle_suffix(target.container.name.as_str())?;
        }
        let path = self.layout.checkout_path(&local, true)?;
        self.check_local_target(&local, Some(&target.container.container_id))?;
        let source = prior
            .map(|track| self.layout.path(&track.path))
            .transpose()?;
        let confirmed = matches!(request.choice, Checkout::Backup);
        let permit_untracked =
            confirmed && request.version.is_none() && request.destination.is_some();
        if kind(&path)?.is_some() {
            check_unique_name(&path)?;
            if source.as_deref() != Some(path.as_path())
                && target.interrupted.is_none()
                && !permit_untracked
            {
                return Err(occupied_path(&path));
            }
        }
        let saved_mirror = target.interrupted.as_ref().and_then(|entry| entry.mirrored);
        let mirrored = match saved_mirror.or(prior.map(|track| track.mirrored)) {
            Some(mirrored) => mirrored,
            None => local == self.destination(target.container)?,
        };
        Ok(Install {
            path,
            local,
            source,
            mirrored,
            confirmed,
            accept_untracked: permit_untracked,
            previous: target.prior,
            kind: if bundle {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            interrupted: target.interrupted,
        })
    }

    pub(super) fn checkout_destination(
        &self,
        request: &CheckoutRequest,
        target: &Selection<'_>,
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
        if let Some(track) = &target.prior {
            return Ok(self
                .moved_mirror(track, target.container)?
                .filter(|path| path.key() != track.path.key())
                .unwrap_or_else(|| track.path.clone()));
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
