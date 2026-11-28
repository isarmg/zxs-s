mod install;
mod preflight;

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    Container, ContainerId, EntryKind, Error, LocalPath, Remote, Result, Track, VersionNum,
    Workspace, local_state,
};
use crate::{
    domain::{ContentHashes, FileState, InterruptedCheckout, Revision},
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
    pub inspect_zip: bool,
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
        let mut interrupted = self.db.interrupted_checkout(&request.id)?;
        let selected = interrupted
            .as_ref()
            .map(|entry| entry.version)
            .or(request.version);
        let (container, version) = match remote.version(&request.id, selected) {
            Err(error @ Error::Remote { status: 404, .. }) if selected.is_none() => {
                if let Some(track) = self
                    .db
                    .track_for_container(&request.id)?
                    .filter(|track| track.endpoint == remote.endpoint())
                {
                    return Err(self.mark_missing(&track.path)?);
                }
                return Err(error);
            }
            result => result?,
        };
        let mut prior = self.db.track_for_container(&request.id)?;
        self.bind_container(remote, &container, (prior.as_mut(), interrupted.as_mut()))?;
        if interrupted.is_none()
            && matches!(request.choice, Checkout::IfClean)
            && let Some(track) = &prior
            && !matches!(&track.hashes, ContentHashes::File(hash)
                if track.restored_from == Some(version.version) && hash == &version.blake3)
            && matches!(local_state(&self.layout, track)?, FileState::Modified)
        {
            return Err(Error::CheckoutConflict);
        }
        self.checkout_selected(
            remote,
            request,
            Selection {
                container: &container,
                version: &DownloadVersion::from(&version),
                inspect_zip: zip_bundle::zip_stem(version.preview_name(&container).as_str())
                    .is_some(),
                prior,
                interrupted,
            },
        )
    }

    pub(super) fn checkout_selected(
        &self,
        remote: &Remote,
        request: CheckoutRequest,
        selection: Selection<'_>,
    ) -> Result<CheckoutResult> {
        let restored_against = preflight::restoration_baseline(&request, &selection)?;
        let version = selection.version;
        let saved_kind = selection.interrupted.as_ref().map(|entry| entry.kind);
        let inspect_zip =
            saved_kind.map_or(selection.inspect_zip, |kind| kind == EntryKind::Directory);
        let staged = if inspect_zip {
            let local = self.checkout_destination(&request, &selection)?;
            let stage = self.checkout_stage(&self.layout.path(&local)?, true)?;
            let payload = stage.child("payload");
            remote.download_version(&request.id, version, &payload)?;
            Some((stage, Bundle::open(&payload)?))
        } else {
            None
        };
        let install =
            self.prepare_install(&request, selection, matches!(&staged, Some((_, Some(_)))))?;
        let job = install::Publication {
            remote,
            id: request.id,
            install,
            version,
            restored_against,
        };
        match staged {
            Some((stage, Some(archive))) => self.publish_bundle(job, &stage, archive),
            Some((stage, None)) => self.publish_file(job, &stage),
            None => self.checkout_regular_file(job),
        }
    }

    fn checkout_stage(&self, path: &Path, use_existing_directory: bool) -> Result<TempDir> {
        let directory = if use_existing_directory && kind(path)? == Some(EntryKind::Directory) {
            path
        } else {
            parent(path)?
        };
        TempDir::create(&self.layout.checkout_storage(directory)?)
    }

    pub(super) fn destination(&self, container: &Container) -> Result<LocalPath> {
        let directory = container.folder.as_str().trim_start_matches('/');
        self.layout
            .local(&Path::new(directory).join(container.name.as_str()))
    }
}
