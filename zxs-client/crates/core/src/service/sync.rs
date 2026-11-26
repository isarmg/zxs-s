use std::{collections::BTreeSet, path::PathBuf};

use serde::Serialize;

use super::{
    Checkout, CheckoutRequest, Container, Error, FileState, FolderPath, LocalPath, Remote, Result,
    Snapshot, Track, Workspace, checkout::Selection, status_one,
};
use crate::{domain::Filter, transport::DownloadVersion};

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
    /// Updates the files already tracked in the workspace.
    ///
    /// # Errors
    /// Returns an error if tracking records or server metadata cannot be read.
    pub fn refresh(&self, remote: &Remote) -> Result<SyncReport> {
        let tracks = self.db.all_tracks()?;
        let containers =
            remote.lookup_containers(tracks.iter().map(|track| &track.container_id))?;
        let mut report = SyncReport::default();
        for track in tracks {
            if let Some(container) = containers.get(&track.container_id) {
                report.record_sync(
                    track.path.as_str().into(),
                    self.sync_container(remote, container, Some(track)),
                );
            } else if track.endpoint == remote.endpoint() {
                if !track.remote_missing {
                    self.db.remote_missing(&track.path, true)?;
                }
                report.remote_missing.push(track.path);
            } else {
                report.issues.push(SyncIssue {
                    path: track.path.as_str().into(),
                    reason: "当前服务器上找不到此追踪项，已保留本地文件".into(),
                });
            }
        }
        report.bundle_warnings = self.bundle_warnings()?;
        Ok(report)
    }

    /// Checks out the selected server directory and updates its locally clean copies.
    ///
    /// # Errors
    /// Returns an error if the selected directory or its inventory cannot be read.
    pub fn sync_directory(&self, remote: &Remote, folder: &FolderPath) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let root = folder == &FolderPath::root();
        let descendants = remote.descendant_folders(folder)?.folders;
        let folders = (!root)
            .then_some(folder.clone())
            .into_iter()
            .chain(descendants.into_iter().map(|folder| folder.folder));
        let bundles = self
            .db
            .all_tracks()?
            .into_iter()
            .filter(|track| matches!(track.snapshot, Snapshot::Bundle(_)))
            .map(|track| track.path.key())
            .collect();
        let mut directories = BTreeSet::new();
        for folder in folders {
            let local = LocalPath::parse(folder.as_str().trim_start_matches('/'))?;
            if let Err(error) = self.sync_folder(&local, &bundles) {
                report.issues.push(SyncIssue {
                    path: local.as_str().into(),
                    reason: error.to_string(),
                });
                continue;
            }
            directories.insert(local);
            if !root {
                self.sync_inventory(remote, Some(folder), &mut report)?;
            }
        }
        if root {
            self.sync_inventory(remote, None, &mut report)?;
        }
        let mut warnings: BTreeSet<_> = self.bundle_warnings()?.into_iter().collect();
        warnings.extend(self.manifests_in(directories)?);
        report.bundle_warnings = warnings.into_iter().collect();
        Ok(report)
    }

    fn sync_inventory(
        &self,
        remote: &Remote,
        folder: Option<FolderPath>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let mut filter = Filter {
            folder,
            ..Filter::default()
        };
        loop {
            let page = remote.list(&filter)?;
            for container in page.containers {
                report.record_sync(
                    format!("{}/{}", container.folder.as_str(), container.name),
                    self.db
                        .track_for_container(&container.container_id)
                        .and_then(|track| self.sync_container(remote, &container, track)),
                );
            }
            let Some(cursor) = page.next_cursor else {
                return Ok(());
            };
            filter.cursor = Some(cursor);
        }
    }

    fn sync_folder(&self, local: &LocalPath, bundles: &BTreeSet<String>) -> Result<()> {
        let mut ancestor = Some(local.clone());
        while let Some(path) = ancestor {
            if bundles.contains(&path.key()) {
                return Err(Error::LocalConflict("服务器文件夹与本地目录包同名".into()));
            }
            ancestor = path.parent();
        }
        std::fs::create_dir_all(self.layout.path(local)?)?;
        Ok(())
    }

    fn sync_container(
        &self,
        remote: &Remote,
        container: &Container,
        mut track: Option<Track>,
    ) -> Result<SyncResult> {
        let mut interrupted = self.db.interrupted_checkout(&container.container_id)?;
        self.bind_container(remote, container, (track.as_mut(), interrupted.as_mut()))?;
        if interrupted.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        if let Some(track) = &mut track {
            track.check_revision(container)?;
            let moved = self.align_mirror(track, container)?;
            if !moved && !matches!(status_one(&self.layout, track)?.state, FileState::Clean) {
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
        let request = CheckoutRequest {
            id: container.container_id.clone(),
            version: Some(container.latest_version),
            destination: track
                .as_ref()
                .filter(|track| !track.mirrored)
                .map(|track| PathBuf::from(track.path.as_str())),
            choice: Checkout::IfClean,
        };
        let version = DownloadVersion {
            number: container.latest_version,
            blake3: container.latest_blake3.clone(),
            size: None,
        };
        self.checkout_selected(
            remote,
            request,
            Selection {
                container,
                version: &version,
                prior: track,
                interrupted,
            },
        )?;
        Ok(SyncResult::Updated)
    }

    fn refresh_track(
        &self,
        remote: &Remote,
        track: &mut Track,
        container: &Container,
    ) -> Result<bool> {
        let mut interrupted = self.db.interrupted_checkout(&container.container_id)?;
        self.bind_container(remote, container, (Some(&mut *track), interrupted.as_mut()))?;
        if interrupted.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        track.check_revision(container)?;
        self.align_mirror(track, container)
    }

    fn align_mirror(&self, track: &mut Track, container: &Container) -> Result<bool> {
        if !track.mirrored {
            return Ok(false);
        }
        let mirror = self.destination(container)?;
        if track.path == mirror
            || (matches!(track.snapshot, Snapshot::Bundle(_))
                && crate::zip_bundle::matches_bundle_stem(&track.path, &mirror))
        {
            return Ok(false);
        }
        self.relocate(track, &mirror)?;
        Ok(true)
    }
}

impl SyncReport {
    fn record_sync(&mut self, path: String, result: Result<SyncResult>) {
        match result {
            Ok(SyncResult::Updated) => self.updated += 1,
            Ok(SyncResult::Current) => self.current += 1,
            Ok(SyncResult::LocalChange) => self.local_changes += 1,
            Err(error) => self.issues.push(SyncIssue {
                path,
                reason: error.to_string(),
            }),
        }
    }

    fn record_track(&mut self, track: super::Track, result: Result<bool>) {
        self.record_sync(
            track.path.into(),
            result.map(|moved| {
                if moved {
                    SyncResult::Updated
                } else {
                    SyncResult::Current
                }
            }),
        );
    }
}
