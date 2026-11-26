mod install;
mod preflight;

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    Container, ContainerId, EntryKind, Error, LocalPath, Remote, Result, Track, VersionNum,
    Workspace,
};
use crate::{
    domain::{InterruptedCheckout, Revision},
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
        let interrupted = self.db.interrupted_checkout(&request.id)?;
        let selected = interrupted
            .as_ref()
            .map(|entry| entry.version)
            .or(request.version);
        let (container, version) = match remote.version(&request.id, selected) {
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
        self.checkout_selected(
            remote,
            request,
            (&container, &DownloadVersion::from(&version)),
        )
    }

    pub(super) fn checkout_selected(
        &self,
        remote: &Remote,
        request: CheckoutRequest,
        (container, version): (&Container, &DownloadVersion),
    ) -> Result<CheckoutResult> {
        let (prior, interrupted) = self.bind_container(remote, container)?;
        let restored_against = preflight::restoration_baseline(
            &request,
            (container, version),
            prior.as_ref(),
            interrupted.as_ref(),
        )?;
        let zip = zip_bundle::zip_stem(container.name.as_str()).is_some();
        let bundle =
            zip && remote.probe_bundle(&request.id, version.number, Some(&version.blake3))?;
        let install = self.prepare_install(
            remote,
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
