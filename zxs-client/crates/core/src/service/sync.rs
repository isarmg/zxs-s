use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{
    Checkout, CheckoutRequest, Container, Error, FileState, FolderPath, LocalPath, Remote, Result,
    Track, Workspace, checkout::Selection, status_one,
};
use crate::{
    domain::{ContainerId, Filter},
    transport::DownloadVersion,
};

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
        let mut pending = Vec::new();
        for track in tracks {
            if let Some(container) = containers.get(&track.container_id) {
                if let Some(target) = self.moved_mirror(&track, container)?
                    && self
                        .db
                        .occupant(&target, &container.container_id)?
                        .is_some()
                {
                    pending.push(container.clone());
                    continue;
                }
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
        self.sync_pending(remote, pending, &mut report)?;
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
        let children = remote.descendant_folders(folder)?.folders;
        let folders = (!root)
            .then_some(folder)
            .into_iter()
            .chain(children.iter().map(|child| &child.folder));
        let tracked = self.db.reserved_paths()?;
        let mut directories = BTreeSet::new();
        let mut blocked = BTreeMap::new();
        for folder in folders {
            let local = LocalPath::parse(folder.as_str().trim_start_matches('/'))?;
            if self.sync_folder(&local, &tracked).is_err() {
                blocked.insert(folder, local);
            } else {
                directories.insert(local);
            }
        }
        let mut deferred = self.sync_inventory(
            remote,
            (!root).then(|| folder.clone()),
            &blocked,
            &mut report,
        )?;
        let mut delayed: Vec<_> = deferred
            .extract_if(.., |entry| blocked.contains_key(&entry.folder))
            .collect();
        self.sync_pending(remote, deferred, &mut report)?;
        if !blocked.is_empty() {
            let tracked = self.db.reserved_paths()?;
            blocked.retain(|_, local| match self.sync_folder(local, &tracked) {
                Ok(()) => {
                    directories.insert(local.clone());
                    false
                }
                Err(error) => {
                    report.issues.push(SyncIssue {
                        path: local.as_str().into(),
                        reason: error.to_string(),
                    });
                    true
                }
            });
        }
        delayed.retain(|entry| !blocked.contains_key(&entry.folder));
        self.sync_pending(remote, delayed, &mut report)?;
        let mut warnings: BTreeSet<_> = self.bundle_warnings()?.into_iter().collect();
        warnings.extend(self.manifests_in(directories)?);
        report.bundle_warnings = warnings.into_iter().collect();
        Ok(report)
    }

    fn sync_inventory(
        &self,
        remote: &Remote,
        folder: Option<FolderPath>,
        blocked: &BTreeMap<&FolderPath, LocalPath>,
        report: &mut SyncReport,
    ) -> Result<Vec<Container>> {
        let mut filter = Filter {
            recursive: folder.is_some(),
            folder,
            ..Filter::default()
        };
        let mut deferred = Vec::new();
        loop {
            let page = remote.list(&filter)?;
            for entry in page.containers {
                if blocked.contains_key(&entry.folder) || self.sync_occupant(&entry)?.is_some() {
                    deferred.push(entry);
                } else {
                    self.sync_entry(remote, &entry, report);
                }
            }
            let Some(cursor) = page.next_cursor else {
                return Ok(deferred);
            };
            filter.cursor = Some(cursor);
        }
    }

    fn sync_occupant(&self, entry: &Container) -> Result<Option<ContainerId>> {
        self.db
            .occupant(&self.destination(entry)?, &entry.container_id)
    }

    fn sync_pending(
        &self,
        remote: &Remote,
        entries: Vec<Container>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let mut pending: BTreeMap<_, _> = entries
            .into_iter()
            .map(|entry| (entry.container_id.clone(), entry))
            .collect();
        while let Some((_, entry)) = pending.pop_first() {
            let mut chain = Vec::new();
            let mut next = Some(entry);
            while let Some(entry) = next {
                next = self
                    .sync_occupant(&entry)?
                    .and_then(|id| pending.remove(&id));
                chain.push(entry);
            }
            for entry in chain.iter().rev() {
                self.sync_entry(remote, entry, report);
            }
        }
        Ok(())
    }

    fn sync_entry(&self, remote: &Remote, container: &Container, report: &mut SyncReport) {
        report.record_sync(
            format!("{}/{}", container.folder.as_str(), container.name),
            self.db
                .track_for_container(&container.container_id)
                .and_then(|track| self.sync_container(remote, container, track)),
        );
    }

    fn sync_folder(&self, local: &LocalPath, tracked: &BTreeSet<String>) -> Result<()> {
        if std::iter::successors(Some(local.clone()), LocalPath::parent)
            .any(|ancestor| tracked.contains(&ancestor.key()))
        {
            return Err(Error::LocalConflict("服务器文件夹与本地追踪项同名".into()));
        }
        fs::create_dir_all(self.layout.checkout_path(local, true)?).map_err(Into::into)
    }

    fn sync_container(
        &self,
        remote: &Remote,
        container: &Container,
        mut track: Option<Track>,
    ) -> Result<SyncResult> {
        self.check_sync(remote, container, track.as_mut())?;
        let mut case_rename = None;
        if let Some(track) = &mut track {
            let current = track.version == container.latest_version;
            let moved = self.moved_mirror(track, container)?;
            if current && let Some(target) = &moved {
                self.relocate(track, target)?;
                return Ok(SyncResult::Updated);
            }
            if !matches!(status_one(&self.layout, track)?.state, FileState::Clean) {
                if moved.is_some() {
                    return Err(Error::LocalConflict(
                        "内容已改变，请先提交或备份再移动".into(),
                    ));
                }
                return Ok(SyncResult::LocalChange);
            }
            if current {
                return Ok(SyncResult::Current);
            }
            case_rename = moved.filter(|target| target.key() == track.path.key());
        }
        let request = CheckoutRequest {
            id: container.container_id.clone(),
            version: Some(container.latest_version),
            destination: track
                .as_ref()
                .filter(|track| !track.mirrored || case_rename.is_some())
                .map(|track| PathBuf::from(track.path.as_str())),
            choice: Checkout::IfClean,
        };
        let version = DownloadVersion {
            number: container.latest_version,
            blake3: container.latest_blake3.clone(),
            size: None,
        };
        let installed = self.checkout_selected(
            remote,
            request,
            Selection {
                container,
                version: &version,
                inspect_zip: super::zip_bundle::zip_stem(container.name.as_str()).is_some(),
                prior: track,
                interrupted: None,
            },
        )?;
        if let Some(target) = case_rename
            && let Some(mut track) = self.db.track(&installed.local)?
        {
            self.relocate(&mut track, &target)?;
        }
        if let Some(backup) = installed.backup.as_deref() {
            discard_sync_backup(backup);
        }
        Ok(SyncResult::Updated)
    }

    fn refresh_track(
        &self,
        remote: &Remote,
        track: &mut Track,
        container: &Container,
    ) -> Result<SyncResult> {
        self.check_sync(remote, container, Some(track))?;
        let Some(mirror) = self.moved_mirror(track, container)? else {
            return Ok(SyncResult::Current);
        };
        self.relocate(track, &mirror)?;
        Ok(SyncResult::Updated)
    }

    fn check_sync(
        &self,
        remote: &Remote,
        container: &Container,
        mut track: Option<&mut Track>,
    ) -> Result<()> {
        let mut interrupted = self.db.interrupted_checkout(&container.container_id)?;
        self.bind_container(
            remote,
            container,
            (track.as_deref_mut(), interrupted.as_mut()),
        )?;
        if interrupted.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        track.map_or(Ok(()), |track| track.check_revision(container))
    }
}

fn discard_sync_backup(path: &Path) {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("could not remove {}: {error}", path.display());
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
}
