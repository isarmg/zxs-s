use std::fs;

use crate::domain::Snapshot;

use super::{
    BundleDirectory, EntryKind, LocalPath, Result, Workspace, has_manifest, local_entry,
    read_directory, relative_display,
};

impl Workspace {
    pub(super) fn browse_bundle(&self, directory: &LocalPath) -> Result<Option<BundleDirectory>> {
        if let Some(root) = self.bundle_root(directory)?
            && let Some(children) = read_directory(&self.layout.path(directory)?)?
        {
            return self.bundle_contents(directory, root, children).map(Some);
        }
        Ok(None)
    }

    pub(in crate::service) fn bundle_root(
        &self,
        directory: &LocalPath,
    ) -> Result<Option<LocalPath>> {
        let checkouts = self.db.all_checkouts()?;
        let ancestors: Vec<_> =
            std::iter::successors(Some(directory.clone()), LocalPath::parent).collect();
        for candidate in ancestors.into_iter().rev() {
            let key = candidate.key();
            if let Some(checkout) = checkouts.iter().find(|checkout| checkout.path.key() == key) {
                return Ok((checkout.kind == EntryKind::Directory).then(|| checkout.path.clone()));
            }
            if let Some(track) = self.db.track(&candidate)? {
                return Ok(matches!(track.snapshot, Snapshot::Bundle(_)).then_some(track.path));
            }
            if candidate.depth() >= 2
                && has_manifest(&self.layout.path(&candidate)?)?
                && self.db.overlapping_path(&candidate, None)?.is_none()
            {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    fn bundle_contents(
        &self,
        directory: &LocalPath,
        root: LocalPath,
        children: fs::ReadDir,
    ) -> Result<BundleDirectory> {
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
        Ok(BundleDirectory {
            root,
            entries,
            symlink_warnings,
        })
    }
}
