use std::{collections::BTreeSet, path::PathBuf};

use serde::Serialize;

use super::{
    Checkout, CheckoutRequest, Container, ContainerId, Error, FileState, FolderPath, LocalPath,
    Remote, Result, Snapshot, Workspace, status_one,
};
use crate::domain::Filter;

mod move_paths;

#[derive(Default, Serialize)]
pub struct SyncReport {
    updated: usize,
    current: usize,
    local_changes: usize,
    pub issues: Vec<SyncIssue>,
    bundle_warnings: Vec<LocalPath>,
    remote_missing: Vec<LocalPath>,
}

#[derive(Serialize)]
pub struct SyncIssue {
    pub path: String,
    pub reason: String,
}

enum SyncResult {
    Updated,
    Current,
    LocalChange,
}

impl Workspace {
    /// Checks out server files and updates locally clean copies to their current version.
    ///
    /// # Errors
    /// Returns an error if the server inventory or pending operations cannot be read.
    pub fn refresh(&self, remote: &Remote) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        self.sync_folders(remote, &mut report)?;
        let mut remote_directories = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut filter = Filter::default();
        loop {
            let page = remote.list(&filter)?;
            for container in page.containers {
                seen.insert(container.container_id.clone());
                let relative = container.folder.as_str().trim_start_matches('/');
                let mut directory = Some(LocalPath::parse(relative)?);
                while let Some(path) = directory {
                    directory = path.parent();
                    remote_directories.insert(path);
                }
                match self.sync_container(remote, &container) {
                    Ok(SyncResult::Updated) => report.updated += 1,
                    Ok(SyncResult::Current) => report.current += 1,
                    Ok(SyncResult::LocalChange) => report.local_changes += 1,
                    Err(error) => report.issues.push(SyncIssue {
                        path: format!(
                            "{}/{}",
                            container.folder.as_str().trim_end_matches('/'),
                            container.name
                        ),
                        reason: error.to_string(),
                    }),
                }
            }
            let Some(cursor) = page.next_cursor else {
                break;
            };
            filter.cursor = Some(cursor);
        }
        let mut warnings: BTreeSet<_> = self.bundle_warnings()?.into_iter().collect();
        warnings.extend(self.manifests_in(remote_directories)?);
        report.bundle_warnings = warnings.into_iter().collect();
        report.remote_missing = self.remote_missing(remote, &seen)?;
        Ok(report)
    }

    fn sync_folders(&self, remote: &Remote, report: &mut SyncReport) -> Result<()> {
        let bundles = self
            .db
            .all_tracks()?
            .into_iter()
            .filter(|track| matches!(track.snapshot, Snapshot::Bundle(_)))
            .map(|track| track.path.key())
            .collect::<BTreeSet<_>>();
        let mut parents = vec![FolderPath::root()];
        while let Some(parent) = parents.pop() {
            let folders = match remote.folders(&parent) {
                Ok(page) => page.folders,
                Err(Error::Remote { status: 404, .. }) if parent != FolderPath::root() => continue,
                Err(error) => return Err(error),
            };
            for folder in folders {
                let path = folder.folder.as_str().trim_start_matches('/');
                match self.sync_folder(path, &bundles) {
                    Ok(()) => parents.push(folder.folder),
                    Err(error) => report.issues.push(SyncIssue {
                        path: path.into(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
        Ok(())
    }

    fn sync_folder(&self, path: &str, bundles: &BTreeSet<String>) -> Result<()> {
        let local = LocalPath::parse(path)?;
        let mut ancestor = Some(local.clone());
        while let Some(path) = ancestor {
            if bundles.contains(&path.key()) {
                return Err(Error::LocalConflict("服务器文件夹与本地目录包同名".into()));
            }
            ancestor = path.parent();
        }
        self.materialize_folder(&self.layout.path(&local)?)
    }

    fn remote_missing(
        &self,
        remote: &Remote,
        seen: &BTreeSet<ContainerId>,
    ) -> Result<Vec<LocalPath>> {
        let mut missing = Vec::new();
        for track in self
            .db
            .all_tracks()?
            .into_iter()
            .filter(|track| track.endpoint == remote.endpoint())
        {
            let unavailable = if seen.contains(&track.container_id) {
                false
            } else {
                match remote.detail(&track.container_id) {
                    Ok(_) => false,
                    Err(Error::Remote { status: 404, .. }) => true,
                    Err(error) => return Err(error),
                }
            };
            if unavailable != track.remote_missing {
                self.db.remote_missing(&track.path, unavailable)?;
            }
            if unavailable {
                missing.push(track.path);
            }
        }
        Ok(missing)
    }

    fn sync_container(&self, remote: &Remote, container: &Container) -> Result<SyncResult> {
        let (mut track, interrupted) = self.bind_container(remote, container)?;
        if interrupted.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        if let Some(track) = &mut track {
            let clean = matches!(status_one(&self.layout, track)?.state, FileState::Clean);
            let moved = self.align_mirror(track, container, clean)?;
            if !clean {
                return Ok(SyncResult::LocalChange);
            }
            if track.version == container.latest_version {
                return Ok(if moved {
                    SyncResult::Updated
                } else {
                    SyncResult::Current
                });
            }
        }
        self.checkout(
            remote,
            CheckoutRequest {
                id: container.container_id.clone(),
                version: Some(container.latest_version),
                destination: track
                    .filter(|track| !track.mirrored)
                    .map(|track| PathBuf::from(track.path.as_str())),
                choice: Checkout::IfClean,
            },
        )?;
        Ok(SyncResult::Updated)
    }

    /// Rechecks the paths and availability of the files already in the workspace.
    /// # Errors
    /// Returns an error if the workspace index cannot be read.
    pub fn refresh_tracked_paths(&self, remote: &Remote) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        self.refresh_tracks(remote, self.db.all_tracks()?, &mut report);
        Ok(report)
    }

    fn refresh_tracks(
        &self,
        remote: &Remote,
        tracks: impl IntoIterator<Item = super::Track>,
        report: &mut SyncReport,
    ) {
        for mut track in tracks {
            match self.refresh_track(remote, &mut track) {
                Ok(Some(true)) => report.updated += 1,
                Ok(Some(false)) if track.remote_missing => report.remote_missing.push(track.path),
                Ok(Some(false)) => report.current += 1,
                Ok(None) => {}
                Err(error) => report.issues.push(SyncIssue {
                    path: track.path.as_str().into(),
                    reason: error.to_string(),
                }),
            }
        }
    }

    fn refresh_track(&self, remote: &Remote, track: &mut super::Track) -> Result<Option<bool>> {
        let container = match remote.detail(&track.container_id) {
            Ok(detail) => detail.container,
            Err(Error::Remote { status: 404, .. }) if track.endpoint != remote.endpoint() => {
                return Ok(None);
            }
            Err(Error::Remote { status: 404, .. }) => {
                self.db.remote_missing(&track.path, true)?;
                track.remote_missing = true;
                return Ok(Some(false));
            }
            Err(error) => return Err(error),
        };
        let (bound, interrupted) = self.bind_container(remote, &container)?;
        if interrupted.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        if let Some(bound) = bound {
            *track = bound;
        }
        let clean = matches!(status_one(&self.layout, track)?.state, FileState::Clean);
        self.align_mirror(track, &container, clean).map(Some)
    }

    fn align_mirror(
        &self,
        track: &mut super::Track,
        container: &Container,
        clean: bool,
    ) -> Result<bool> {
        if !track.mirrored {
            return Ok(false);
        }
        let mirror = self.destination(
            container,
            None,
            matches!(track.snapshot, Snapshot::Bundle(_)),
        )?;
        if track.path == mirror {
            return Ok(false);
        }
        if !clean {
            return Err(Error::LocalConflict(format!(
                "远端已移到 {}。请先处理 {} 的本地变更",
                mirror.as_str(),
                track.path.as_str()
            )));
        }
        self.relocate(track, &mirror)?;
        Ok(true)
    }
}
