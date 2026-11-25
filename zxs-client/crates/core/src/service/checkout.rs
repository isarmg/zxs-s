use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{
    Container, ContainerId, EntryKind, Error, FileState, LocalPath, Remote, Result, Track,
    VersionNum, Workspace, check_endpoint, invalid, kind, status_one,
};
use crate::zip_bundle::{self, Apply, Bundle, Checkout};

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

impl Workspace {
    /// # Errors
    ///
    /// Returns an error on server or integrity failures or when checkout would overwrite untracked content or unbacked changes.
    pub fn checkout(&self, remote: &Remote, request: CheckoutRequest) -> Result<CheckoutResult> {
        let (container, version) = remote.version(&request.id, request.version)?;
        let mut apply = Apply::new(self.layout.staged()?);
        remote.download_new(&request.id, &version, &apply.payload())?;
        let mut bundle = Bundle::open(&apply.payload())?;
        let local = self.destination(
            remote,
            &container,
            request.destination.as_deref(),
            bundle.is_some(),
        )?;
        self.unoccupied(&local)?;
        if let Some(pending) = self.db.pending(&local)? {
            self.reconcile(remote, pending)?;
        }
        let previous = self.db.track(&local)?;
        let path = self.layout.path(&local)?;
        let backup = self.checkout_backup(&path, previous.as_ref(), &request, remote)?;
        let snapshot = match &mut bundle {
            Some(bundle) => apply.install_bundle(
                &path,
                bundle,
                previous.as_ref().map(|track| &track.snapshot),
            )?,
            None => apply.install_file(&path, previous.is_some())?,
        };
        self.db.save(&Track {
            path: local.clone(),
            endpoint: remote.endpoint().into(),
            container_id: request.id,
            version: version.version,
            snapshot,
        })?;
        apply.commit();
        Ok(CheckoutResult {
            path,
            local,
            version: version.version,
            backup,
            bundle: bundle.is_some(),
        })
    }

    fn destination(
        &self,
        remote: &Remote,
        container: &Container,
        path: Option<&Path>,
        bundle: bool,
    ) -> Result<LocalPath> {
        if let Some(path) = path {
            return self.layout.local(path);
        }
        if let Some(track) = self.db.all_tracks()?.into_iter().find(|track| {
            track.container_id == container.container_id && track.endpoint == remote.endpoint()
        }) {
            return Ok(track.path);
        }
        let name = container.name.as_str();
        if bundle {
            let stem = name
                .strip_suffix(".zip")
                .or_else(|| name.strip_suffix(".ZIP"))
                .filter(|stem| !stem.is_empty());
            let name = stem.map_or_else(|| format!("{name}.bundle"), str::to_owned);
            self.layout.local(Path::new(&name))
        } else {
            self.layout.local(Path::new(name))
        }
    }

    fn checkout_backup(
        &self,
        path: &Path,
        prior: Option<&Track>,
        request: &CheckoutRequest,
        remote: &Remote,
    ) -> Result<Option<PathBuf>> {
        let Some(track) = prior else { return Ok(None) };
        check_endpoint(remote, &track.endpoint)?;
        if track.container_id != request.id {
            return Err(Error::LocalConflict(
                "local path is tracked by a different container".into(),
            ));
        }
        match status_one(&self.layout, track)?.state {
            FileState::Missing | FileState::Clean => Ok(None),
            _ => match request.choice {
                Checkout::IfClean => Err(Error::LocalConflict(
                    "local changes need to be committed or backed up before checkout".into(),
                )),
                Checkout::Backup => self.back_up(path).map(Some),
            },
        }
    }

    fn back_up(&self, path: &Path) -> Result<PathBuf> {
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
