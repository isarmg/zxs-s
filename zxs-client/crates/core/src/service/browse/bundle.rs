use std::{fs, path::Path};

use crate::domain::{ContainerId, EntryKind, InterruptedCheckout, LocalPath, Result, Snapshot};

use super::{BundleDirectory, Workspace, WorkspacePage, has_manifest, local_entry};

impl Workspace {
    pub(super) fn bundle_root(
        &self,
        directory: &LocalPath,
        checkouts: &[InterruptedCheckout],
    ) -> Result<Option<(LocalPath, Option<ContainerId>)>> {
        let ancestors: Vec<_> =
            std::iter::successors(Some(directory.clone()), LocalPath::parent).collect();
        for candidate in ancestors.into_iter().rev() {
            let key = candidate.key();
            if let Some(checkout) = checkouts.iter().find(|checkout| checkout.path.key() == key) {
                return Ok((checkout.kind == EntryKind::Directory)
                    .then(|| (checkout.path.clone(), Some(checkout.container_id.clone()))));
            }
            if let Some(track) = self.db.track(&candidate)? {
                return Ok(matches!(track.snapshot, Snapshot::Bundle(_))
                    .then_some((track.path, Some(track.container_id))));
            }
            if candidate.depth() >= 2
                && has_manifest(&self.layout.path(&candidate)?)?
                && self.db.overlapping_path(&candidate, None)?.is_none()
            {
                return Ok(Some((candidate, None)));
            }
        }
        Ok(None)
    }

    pub(super) fn bundle_contents(
        &self,
        directory: &LocalPath,
        (root, container_id): (LocalPath, Option<ContainerId>),
        children: fs::ReadDir,
    ) -> Result<WorkspacePage> {
        let mut entries = Vec::new();
        let mut symlink_warnings = Vec::new();
        for child in children {
            let child = child?;
            let file_type = child.file_type()?;
            if file_type.is_symlink() {
                symlink_warnings.push(relative_display(&self.layout.root, &child.path()));
            } else if let Some(entry) = local_entry(directory, &child.file_name(), file_type) {
                entries.push(entry);
            }
        }
        entries.sort_unstable_by(|left, right| {
            (left.kind != EntryKind::Directory, &left.path)
                .cmp(&(right.kind != EntryKind::Directory, &right.path))
        });
        Ok(WorkspacePage::Bundle(BundleDirectory {
            root,
            container_id,
            entries,
            symlink_warnings,
        }))
    }
}

pub(super) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
