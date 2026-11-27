use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

use super::{
    Catalog, Error, FileName, FolderPath, LocalPath, Remote, Result, Snapshot, Workspace, invalid,
    zip_bundle,
};
use crate::{
    domain::{InterruptedCheckout, Track, file_name_key},
    store::layout::IGNORE_FILE,
};

impl Workspace {
    /// Creates the ignore manifest for a directory that can be managed as a ZIP.
    ///
    /// # Errors
    /// Returns an error when another container occupies the directory or the manifest cannot be written.
    pub fn init_bundle(&self, path: &Path, remote: &Remote) -> Result<LocalPath> {
        let local = self.layout.local(path)?;
        let catalog = Catalog::open(remote, None)?;
        let parent = check_bundle_location(&local, &catalog)?;
        if self.db.track(&local)?.is_some()
            || self
                .db
                .all_checkouts()?
                .iter()
                .any(|entry| entry.path == local)
        {
            return Err(bundle_conflict(&local));
        }
        self.check_bundle_directory(&local, None)?;
        let name = zip_bundle::bundle_name(&FileName::parse(local.name())?)?;
        let target = FolderPath::parse(format!("{parent}/{name}"))?;
        let key = file_name_key(target.as_str());
        if catalog.find_name(&parent, &name)?.is_some()
            || catalog
                .children(&parent)?
                .iter()
                .any(|child| file_name_key(child.folder.as_str()) == key)
        {
            return Err(Error::LocalConflict(format!("远端路径 {target} 已被占用")));
        }
        let path = self.layout.path(&local)?;
        zip_bundle::init_bundle(&path)?;
        Ok(local)
    }

    pub(super) fn check_bundle_directory(
        &self,
        directory: &LocalPath,
        track: Option<&Track>,
    ) -> Result<()> {
        if track.is_some_and(|track| !matches!(track.snapshot, Snapshot::Bundle(_))) {
            return Err(bundle_conflict(directory));
        }
        match self.unoccupied(directory) {
            Err(Error::LocalConflict(_)) => Err(bundle_conflict(directory)),
            result => result,
        }
    }

    /// Returns directories whose ignore manifest overlaps independently tracked containers.
    ///
    /// # Errors
    /// Returns an error if the tracking index or manifest location cannot be inspected.
    pub fn bundle_warnings(&self) -> Result<Vec<LocalPath>> {
        self.bundle_warnings_for(&self.db.all_tracks()?, &self.db.all_checkouts()?)
    }

    pub(super) fn bundle_warnings_for(
        &self,
        tracks: &[Track],
        checkouts: &[InterruptedCheckout],
    ) -> Result<Vec<LocalPath>> {
        let mut directories = BTreeSet::new();
        directories.extend(
            tracks
                .iter()
                .filter(|entry| matches!(entry.snapshot, Snapshot::File(_)))
                .map(|entry| entry.path.clone()),
        );
        for path in tracks
            .iter()
            .map(|entry| &entry.path)
            .chain(checkouts.iter().map(|entry| &entry.path))
        {
            let mut parent = path.parent();
            while let Some(directory) = parent {
                parent = directory.parent();
                directories.insert(directory);
            }
        }
        self.manifests_in(directories)
    }

    pub(super) fn manifests_in(&self, directories: BTreeSet<LocalPath>) -> Result<Vec<LocalPath>> {
        let mut warnings = Vec::new();
        for directory in directories {
            let path = match self.layout.path(&directory) {
                Ok(path) => path.join(IGNORE_FILE),
                Err(Error::SymbolicLink(_)) => continue,
                Err(error) => return Err(error),
            };
            match fs::symlink_metadata(path) {
                Ok(_) => warnings.push(directory),
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(warnings)
    }
}

pub(super) fn check_bundle_location(
    local: &LocalPath,
    catalog: &Catalog<'_>,
) -> Result<FolderPath> {
    let parent = local
        .parent()
        .filter(|path| !path.is_root())
        .ok_or_else(|| invalid("目录包须在已登记的数据盘内创建，已忽略 .zxsignore"))?;
    let folder = FolderPath::parse(format!("/{}", parent.as_str()))?;
    catalog.check_disk(&folder)?;
    Ok(folder)
}

fn bundle_conflict(directory: &LocalPath) -> Error {
    Error::LocalConflict(format!(
        "目录 {} 与已有文件容器重叠，已忽略 .zxsignore",
        directory.as_str()
    ))
}
