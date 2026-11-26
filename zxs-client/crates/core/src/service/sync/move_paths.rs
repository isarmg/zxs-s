use std::path::Path;

use crate::{
    domain::{ContainerId, Folder, FolderMove, Patch, RecycleId, Restore},
    service::Catalog,
    store::layout::Layout,
};

use super::{Container, LocalPath, Remote, Result, SyncIssue, SyncReport, Workspace};

impl Workspace {
    /// Updates the remote file and follows the resulting path for its tracked copy.
    ///
    /// # Errors
    /// Returns an error if the server rejects the update. Local issues are returned in the report.
    pub fn patch_file(
        root: &Path,
        remote: &Remote,
        id: &ContainerId,
        patch: &Patch,
    ) -> Result<(Container, SyncReport)> {
        let container = remote.patch(id, patch)?;
        let report = if patch.folder.is_some() || patch.name.is_some() {
            follow_file(root, remote, &container)
        } else {
            SyncReport::default()
        };
        Ok((container, report))
    }

    /// Restores a remote file and follows the resulting path for its tracked copy.
    ///
    /// # Errors
    /// Returns an error if the server rejects the restore. Local issues are returned in the report.
    pub fn restore_file(
        root: &Path,
        remote: &Remote,
        id: &RecycleId,
        restore: &Restore,
    ) -> Result<(Container, SyncReport)> {
        let container = remote.restore(id, restore)?;
        let report = follow_file(root, remote, &container);
        Ok((container, report))
    }

    /// Moves a server folder and follows the new paths of its clean tracked files.
    ///
    /// # Errors
    /// Returns an error if the workspace or server move cannot be prepared or accepted.
    /// Local synchronization issues after an accepted move are returned in the report.
    pub fn move_folder(
        root: &Path,
        remote: &Remote,
        change: &FolderMove,
    ) -> Result<(Folder, SyncReport)> {
        let catalog = Catalog::open(remote, None)?;
        change.from.require_child()?;
        change.to.require_child()?;
        let mut report = SyncReport::default();
        let Some(workspace) = open_existing(root)? else {
            return Ok((catalog.move_folder(change)?, report));
        };
        let source = LocalPath::parse(change.from.as_str().trim_start_matches('/'))?;
        let tracks = workspace.db.tracks_in(&source)?;
        let moved = catalog.move_folder(change)?;
        match remote.lookup_containers(tracks.iter().map(|track| &track.container_id)) {
            Ok(containers) => {
                let target = moved.folder.as_str();
                let prefix = format!("{target}/");
                for mut track in tracks {
                    let Some(container) = containers.get(&track.container_id) else {
                        continue;
                    };
                    let folder = container.folder.as_str();
                    if folder == target || folder.starts_with(&prefix) {
                        let result = workspace.refresh_track(remote, &mut track, container);
                        report.record_track(track, result);
                    }
                }
            }
            Err(error) => report.issues.push(SyncIssue {
                path: moved.folder.as_str().into(),
                reason: error.to_string(),
            }),
        }
        Ok((moved, report))
    }
}

fn follow_file(root: &Path, remote: &Remote, container: &Container) -> SyncReport {
    let mut report = SyncReport::default();
    let follow = || -> Result<_> {
        let Some(workspace) = open_existing(root)? else {
            return Ok(None);
        };
        Ok(workspace
            .db
            .track_for_container(&container.container_id)?
            .map(|mut track| {
                let result = workspace.refresh_track(remote, &mut track, container);
                (track, result)
            }))
    };
    match follow() {
        Ok(Some((track, result))) => report.record_track(track, result),
        Ok(None) => {}
        Err(error) => report.issues.push(SyncIssue {
            path: format!("{}/{}", container.folder.as_str(), container.name),
            reason: error.to_string(),
        }),
    }
    report
}

fn open_existing(root: &Path) -> Result<Option<Workspace>> {
    Layout::index_at(root)
        .try_exists()?
        .then(|| Workspace::open(root))
        .transpose()
}
