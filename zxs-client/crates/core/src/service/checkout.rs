mod install;
mod preflight;

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    Container, ContainerId, EntryKind, Error, LocalPath, Remote, Result, Track, VersionNum,
    Workspace,
};
use crate::{
    domain::{InterruptedCheckout, Revision, Version},
    store::layout::{TempDir, kind, parent},
    transport::DownloadVersion,
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
    pub restored_from: Option<VersionNum>,
}

pub(super) struct Selection<'a> {
    pub container: &'a Container,
    pub version: &'a DownloadVersion,
    pub prior: Option<Track>,
    pub interrupted: Option<InterruptedCheckout>,
}

struct Install {
    path: PathBuf,
    local: LocalPath,
    previous: Option<Track>,
    source: Option<PathBuf>,
    kind: EntryKind,
    confirmed: bool,
    accept_untracked: bool,
    interrupted: Option<InterruptedCheckout>,
    mirrored: bool,
}

impl Workspace {
    /// # Errors
    ///
    /// Returns an error on server or integrity failures or if local data cannot be safely replaced.
    pub fn checkout(&self, remote: &Remote, request: CheckoutRequest) -> Result<CheckoutResult> {
        self.checkout_version(remote, request, None)
    }

    pub(super) fn checkout_version(
        &self,
        remote: &Remote,
        request: CheckoutRequest,
        known: Option<(Container, Version)>,
    ) -> Result<CheckoutResult> {
        let mut interrupted = self.db.interrupted_checkout(&request.id)?;
        let selected = interrupted
            .as_ref()
            .map(|entry| entry.version)
            .or(request.version);
        let metadata = known
            .filter(|(_, version)| selected.is_none_or(|number| number == version.version))
            .map_or_else(|| remote.version(&request.id, selected), Ok);
        let (container, version) = match metadata {
            Err(Error::Remote { status: 404, .. }) if selected.is_none() => {
                if let Some(track) = self
                    .db
                    .track_for_container(&request.id)?
                    .filter(|track| track.endpoint == remote.endpoint())
                {
                    return Err(self.mark_missing(&track.path)?);
                }
                return Err(Error::Remote {
                    status: 404,
                    message: "container not found".into(),
                });
            }
            result => result?,
        };
        let mut prior = self.db.track_for_container(&request.id)?;
        self.bind_container(remote, &container, (prior.as_mut(), interrupted.as_mut()))?;
        self.checkout_selected(
            remote,
            request,
            Selection {
                container: &container,
                version: &DownloadVersion::from(&version),
                prior,
                interrupted,
            },
        )
    }

    pub(super) fn checkout_selected(
        &self,
        remote: &Remote,
        request: CheckoutRequest,
        Selection {
            container,
            version,
            prior,
            interrupted,
        }: Selection<'_>,
    ) -> Result<CheckoutResult> {
        let restored_against = preflight::restoration_baseline(
            &request,
            (container, version),
            prior.as_ref(),
            interrupted.as_ref(),
        )?;
        let zip = zip_bundle::zip_stem(container.name.as_str()).is_some();
        let bundle = zip && remote.probe_bundle(&request.id, version.number, &version.blake3)?;
        let install = self.prepare_install(
            &request,
            preflight::Target {
                container,
                prior,
                interrupted,
                bundle,
            },
        )?;
        let directory = if bundle && kind(&install.path)? == Some(EntryKind::Directory) {
            &install.path
        } else {
            parent(&install.path)?
        };
        let stage = TempDir::create(&self.layout.checkout_storage(directory)?)?;
        remote.download_version(&request.id, version, &stage.child("payload"))?;
        let archive = if zip {
            Bundle::open(&stage.child("payload"))?
        } else {
            None
        };
        if archive.is_some() != bundle {
            return Err(Error::LocalConflict("目录包标记与所选原件不一致".into()));
        }
        self.publish_checkout(install::Publication {
            remote,
            id: request.id,
            install,
            stage,
            archive,
            version,
            restored_against,
        })
    }

    pub(super) fn destination(&self, container: &Container) -> Result<LocalPath> {
        let directory = container.folder.as_str().trim_start_matches('/');
        self.layout
            .local(&Path::new(directory).join(container.name.as_str()))
    }
}
