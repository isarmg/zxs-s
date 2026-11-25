mod preflight;

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{
    Catalog, Container, ContainerId, EntryKind, Error, FileState, LocalPath, Remote, Result, Track,
    VersionNum, Workspace, invalid, kind, status_one,
};
use crate::{
    domain::{ContentHashes, InterruptedCheckout, Version},
    store::layout::{TempDir, parent, publish_staged_file, stamp},
    zip_bundle::{self, Bundle, Checkout},
};

pub struct CheckoutRequest {
    pub id: ContainerId,
    pub version: Option<VersionNum>,
    pub destination: Option<PathBuf>,
    pub choice: Checkout,
}

#[derive(Serialize)]
pub struct CheckoutResult {
    pub path: PathBuf,
    pub local: LocalPath,
    pub version: VersionNum,
    pub backup: Option<PathBuf>,
    pub bundle: bool,
}

struct Install {
    path: PathBuf,
    track_path: LocalPath,
    previous: Option<Track>,
    version: Version,
    mirrored: bool,
    backup: Option<PathBuf>,
    retry: bool,
}

impl Workspace {
    /// # Errors
    ///
    /// Returns an error on server or integrity failures or when checkout would overwrite untracked content or unbacked changes.
    pub fn checkout(&self, remote: &Remote, request: CheckoutRequest) -> Result<CheckoutResult> {
        let (container, version) = self.checkout_version(remote, &request)?;
        let (_, interrupted) = self.bind_container(remote, &container)?;
        if interrupted.is_some() && !matches!(request.choice, Checkout::Backup) {
            return Err(Error::CheckoutInterrupted);
        }
        let stage = self.layout.staged()?;
        let payload = stage.child("payload");
        remote.download_new(&request.id, &version, &payload)?;
        let bundle = if zip_bundle::zip_stem(container.name.as_str()).is_some() {
            Bundle::open(&payload)?
        } else {
            None
        };
        let install =
            self.prepare_install(remote, &request, (&container, version), bundle.is_some())?;
        match bundle {
            Some(bundle) => self.install_bundle(remote, request.id, install, &stage, bundle),
            None => self.install_file(remote, request.id, install, &stage),
        }
    }

    fn checkout_version(
        &self,
        remote: &Remote,
        request: &CheckoutRequest,
    ) -> Result<(Container, Version)> {
        match remote.version(&request.id, request.version) {
            Err(Error::Remote { status: 404, .. }) if request.version.is_none() => {
                if let Some(track) = self
                    .db
                    .track_for_container(&request.id)?
                    .filter(|track| track.endpoint == remote.endpoint())
                {
                    return Err(self.mark_missing(&track.path)?);
                }
                Err(Error::Remote {
                    status: 404,
                    message: "container not found".into(),
                })
            }
            result => result,
        }
    }

    fn prepare_install(
        &self,
        remote: &Remote,
        request: &CheckoutRequest,
        (container, version): (&Container, Version),
        bundle: bool,
    ) -> Result<Install> {
        if bundle && request.destination.is_none() {
            Catalog::open(remote, None)?.check_bundle_name(&container.folder, &container.name)?;
        }
        let interrupted = self.db.interrupted_checkout(&request.id)?;
        let local = if let Some(previous) = &interrupted {
            let path = request
                .destination
                .as_ref()
                .map(|path| self.layout.local(path))
                .transpose()?;
            if path.is_some_and(|path| path != previous.path) {
                return Err(Error::CheckoutInterrupted);
            }
            previous.path.clone()
        } else {
            self.destination(container, request.destination.as_deref(), bundle)?
        };
        self.align_checkout_target(request, &local)?;
        if bundle {
            self.check_bundle_directory(&local, remote)?;
        } else {
            self.unoccupied(&local)?;
        }
        let previous = self.db.track(&local)?;
        let path = self.layout.path(&local)?;
        let recovering = interrupted.is_some()
            || (previous.is_none()
                && request.destination.is_some()
                && matches!(request.choice, Checkout::Backup));
        let backup = if recovering && kind(&path)?.is_some() {
            Some(self.back_up(&path)?)
        } else {
            self.checkout_backup(&path, previous.as_ref(), request)?
        };
        let mirrored = previous.as_ref().is_some_and(|track| track.mirrored)
            || local == self.destination(container, None, bundle)?;
        Ok(Install {
            path,
            track_path: local,
            previous,
            version,
            mirrored,
            backup,
            retry: recovering,
        })
    }

    fn align_checkout_target(&self, request: &CheckoutRequest, local: &LocalPath) -> Result<()> {
        let Some(mut track) = self.db.track_for_container(&request.id)? else {
            return Ok(());
        };
        if &track.path == local {
            return Ok(());
        }
        if request.destination.is_none()
            && track.mirrored
            && matches!(status_one(&self.layout, &track)?.state, FileState::Clean)
        {
            return self.relocate(&mut track, local);
        }
        Err(Error::LocalConflict(format!(
            "此容器已追踪到 {}，可重新检出到原位置或先解除追踪",
            track.path.as_str()
        )))
    }

    fn install_file(
        &self,
        remote: &Remote,
        id: ContainerId,
        install: Install,
        stage: &TempDir,
    ) -> Result<CheckoutResult> {
        preflight::verify_replacement(&install)?;
        fs::create_dir_all(parent(&install.path)?)?;
        self.mark_interrupted(remote, &id, &install, EntryKind::File)?;
        publish_staged_file(
            &stage.child("payload"),
            &install.path,
            &stage.id()?,
            &install.version.blake3,
        )?;
        let snapshot = super::Snapshot::File(stamp(&install.path)?);
        self.db.save(&Track {
            path: install.track_path.clone(),
            endpoint: remote.endpoint().into(),
            container_id: id,
            version: install.version.version,
            snapshot,
            hashes: ContentHashes::File(install.version.blake3),
            remote_missing: false,
            mirrored: install.mirrored,
        })?;
        Ok(CheckoutResult {
            path: install.path,
            local: install.track_path,
            version: install.version.version,
            backup: install.backup,
            bundle: false,
        })
    }

    fn install_bundle(
        &self,
        remote: &Remote,
        id: ContainerId,
        install: Install,
        stage: &TempDir,
        mut bundle: Bundle,
    ) -> Result<CheckoutResult> {
        let expanded = stage.child("expanded");
        let plan = zip_bundle::plan_bundle(
            &install.path,
            &expanded,
            &mut bundle,
            install.previous.as_ref().map(|track| &track.snapshot),
            install.retry,
        )?;
        if install.backup.is_none()
            && let Some(Track {
                hashes: ContentHashes::Bundle(hashes),
                ..
            }) = &install.previous
        {
            for (member, file) in &plan.files {
                if file.old.is_some() && hashes.get(member) != file.old.as_ref() {
                    return Err(Error::CheckoutConflict);
                }
            }
        }
        self.mark_interrupted(remote, &id, &install, EntryKind::Directory)?;
        let (snapshot, hashes) =
            zip_bundle::apply_bundle(&install.path, &expanded, &stage.id()?, &plan)?;
        self.db.save(&Track {
            path: install.track_path.clone(),
            endpoint: remote.endpoint().into(),
            container_id: id,
            version: install.version.version,
            snapshot,
            hashes,
            remote_missing: false,
            mirrored: install.mirrored,
        })?;
        Ok(CheckoutResult {
            path: install.path,
            local: install.track_path,
            version: install.version.version,
            backup: install.backup,
            bundle: true,
        })
    }

    fn mark_interrupted(
        &self,
        remote: &Remote,
        id: &ContainerId,
        install: &Install,
        kind: EntryKind,
    ) -> Result<()> {
        self.db.start_checkout(&InterruptedCheckout {
            path: install.track_path.clone(),
            container_id: id.clone(),
            version: install.version.version,
            endpoint: remote.endpoint().into(),
            kind,
        })
    }

    pub(super) fn destination(
        &self,
        container: &Container,
        path: Option<&Path>,
        bundle: bool,
    ) -> Result<LocalPath> {
        if let Some(path) = path {
            return self.layout.local(path);
        }
        let name = container.name.as_str();
        let name = if bundle {
            zip_bundle::zip_stem(name)
                .filter(|stem| !stem.is_empty())
                .unwrap_or(name)
        } else {
            name
        };
        let directory = container.folder.as_str().trim_start_matches('/');
        self.layout.local(&Path::new(directory).join(name))
    }
}
