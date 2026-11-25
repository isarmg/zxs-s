use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

use crate::{
    domain::{Folder, FolderMove, Patch},
    service::{Catalog, EntryKind, invalid, kind},
    store::layout::Layout,
};

use super::{
    Container, ContainerId, FolderPath, LocalPath, Remote, Result, Snapshot, SyncIssue, SyncReport,
    Workspace,
};

impl Workspace {
    /// Updates the file and moves its clean workspace copy to the resulting server path.
    ///
    /// # Errors
    /// Returns an error if the workspace cannot be opened or the server rejects the update.
    /// Local synchronization issues after an accepted update are returned in the report.
    pub fn patch_file(
        root: &Path,
        remote: &Remote,
        id: &ContainerId,
        patch: &Patch,
    ) -> Result<(Container, SyncReport)> {
        let catalog = Catalog::open(remote, Some(root))?;
        let workspace = if patch.folder.is_some() || patch.name.is_some() {
            open_existing(root)?
        } else {
            None
        };
        let track = workspace
            .as_ref()
            .map(|workspace| workspace.db.track_for_container(id))
            .transpose()?
            .flatten();
        let updated = catalog.patch(id, patch)?;
        let mut report = SyncReport::default();
        if let (Some(workspace), Some(track)) = (workspace, track) {
            workspace.refresh_tracks(remote, [track], &mut report);
        }
        Ok((updated, report))
    }

    /// Moves a server folder and updates its local directory tree and clean mirrored files.
    ///
    /// # Errors
    /// Returns an error if the paths, workspace, or server move cannot be prepared or accepted.
    /// Local synchronization issues after an accepted move are returned in the report.
    pub fn move_folder(
        root: &Path,
        remote: &Remote,
        change: &FolderMove,
    ) -> Result<(Folder, SyncReport)> {
        let catalog = Catalog::open(remote, Some(root))?;
        change.from.require_child()?;
        change.to.require_child()?;
        let mut report = SyncReport::default();
        let Some(workspace) = open_existing(root)? else {
            return Ok((catalog.move_folder(change)?, report));
        };
        let source = local_folder(&change.from)?;
        let tracks = workspace.db.tracks_in(&source)?;
        let bundles = workspace
            .db
            .all_tracks()?
            .into_iter()
            .filter(|track| matches!(track.snapshot, Snapshot::Bundle(_)))
            .map(|track| track.path.key())
            .collect::<BTreeSet<_>>();
        let directories = folder_tree(&catalog, &change.from)?;
        let moved = catalog.move_folder(change)?;
        workspace.refresh_tracks(remote, tracks, &mut report);

        // Recreate empty directories at the destination, then remove empty source directories.
        let mut originals = Vec::new();
        for folder in directories {
            match workspace.mirror_folder_path(&folder, &change.from, &moved.folder, &bundles) {
                Ok(pair) => originals.push(pair),
                Err(error) => report.issues.push(SyncIssue {
                    path: folder.as_str().into(),
                    reason: error.to_string(),
                }),
            }
        }
        for (original, mirror) in originals.into_iter().rev() {
            if let Err(error) = workspace.remove_empty_source(&original, &mirror) {
                report.issues.push(SyncIssue {
                    path: original.as_str().into(),
                    reason: error.to_string(),
                });
            }
        }
        Ok((moved, report))
    }

    fn mirror_folder_path(
        &self,
        folder: &FolderPath,
        from: &FolderPath,
        to: &FolderPath,
        bundles: &BTreeSet<String>,
    ) -> Result<(LocalPath, LocalPath)> {
        let suffix = folder
            .as_str()
            .strip_prefix(from.as_str())
            .ok_or_else(|| invalid("服务器返回的目录不在原目录内"))?;
        let original = local_folder(folder)?;
        let mirror = LocalPath::parse(format!("{}{suffix}", to.as_str().trim_start_matches('/')))?;
        self.sync_folder(mirror.as_str(), bundles)?;
        Ok((original, mirror))
    }

    fn remove_empty_source(&self, original: &LocalPath, mirror: &LocalPath) -> Result<()> {
        let old = self.layout.path(original)?;
        if kind(&old)? != Some(EntryKind::Directory) {
            return Ok(());
        }
        let new = self.layout.path(mirror)?;
        if old == new
            || (original.key() == mirror.key() && old.canonicalize()? == new.canonicalize()?)
        {
            return Ok(());
        }
        match fs::remove_dir(old) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::DirectoryNotEmpty | ErrorKind::NotFound
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn open_existing(root: &Path) -> Result<Option<Workspace>> {
    Layout::index_at(root)
        .try_exists()?
        .then(|| Workspace::open(root))
        .transpose()
}

fn local_folder(folder: &FolderPath) -> Result<LocalPath> {
    LocalPath::parse(folder.as_str().trim_start_matches('/'))
}

fn folder_tree(catalog: &Catalog<'_>, root: &FolderPath) -> Result<Vec<FolderPath>> {
    let mut folders = vec![root.clone()];
    let mut index = 0;
    while let Some(parent) = folders.get(index) {
        folders.extend(
            catalog
                .children(parent)?
                .into_iter()
                .map(|entry| entry.folder),
        );
        index += 1;
    }
    Ok(folders)
}
