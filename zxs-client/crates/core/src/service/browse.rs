use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{self, ErrorKind},
    path::Path,
};

use serde::Serialize;

use super::{EntryKind, FileState, LocalPath, Result, Status, Workspace, inspect, kind};
use crate::{ContainerId, store::layout::IGNORE_FILE};

mod bundle;
mod scan;

use scan::Scan;

const MAX_UNTRACKED: usize = 1000;

pub(super) enum ScanPurpose {
    Status,
    Browse,
    Commit,
}
#[derive(Serialize)]
pub struct WorkspaceEntry {
    #[serde(flatten)]
    pub(super) status: Status,
    pub(super) local_kind: Option<EntryKind>,
    pub(super) can_submit: bool,
}

#[derive(Serialize)]
pub struct WorkspaceStatus {
    pub entries: Vec<Status>,
    pub bundle_warnings: Vec<LocalPath>,
    pub truncated: bool,
    pub symlink_warnings: Vec<String>,
}

pub(super) struct DirectoryScan {
    pub(super) status: WorkspaceStatus,
    pub(super) empty_directories: Vec<LocalPath>,
    pub(super) name_conflicts: HashSet<(LocalPath, String)>,
}

#[derive(Serialize)]
pub struct WorkspaceChanges {
    pub(super) entries: Vec<WorkspaceEntry>,
    bundle_warnings: Vec<LocalPath>,
    symlink_warnings: Vec<String>,
    truncated: bool,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorkspacePage {
    Changes(WorkspaceChanges),
    Bundle(BundleDirectory),
}

#[derive(Serialize)]
pub struct BundleDirectory {
    pub(super) root: LocalPath,
    container_id: Option<ContainerId>,
    pub(super) entries: Vec<DirectoryEntry>,
    symlink_warnings: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct DirectoryEntry {
    pub(super) path: LocalPath,
    pub(super) kind: EntryKind,
}

impl Workspace {
    /// Scans tracked entries and new local files or initialized directory workspaces.
    ///
    /// # Errors
    /// Returns an error if the workspace or the tracking index cannot be scanned.
    pub fn status(&self) -> Result<WorkspaceStatus> {
        self.scan_directory(&LocalPath::parse(".")?, ScanPurpose::Status)
            .map(|scan| scan.status)
    }

    pub(super) fn scan_directory(
        &self,
        directory: &LocalPath,
        purpose: ScanPurpose,
    ) -> Result<DirectoryScan> {
        let tracks = self.db.tracks_in(directory)?;
        let checkouts = self.db.all_checkouts()?;
        let mut warnings: BTreeSet<_> = self
            .bundle_warnings_for(&tracks, &checkouts)?
            .into_iter()
            .filter(|path| contains(directory, path))
            .collect();
        let scope = (directory, matches!(purpose, ScanPurpose::Commit));
        let (mut entries, mut links) = inspect::status(&self.layout, &tracks, checkouts, scope)?;
        let (parents, tracked) = tracked_paths(entries.iter().map(|entry| &entry.path));
        let mut scanner = Scan::new(&self.layout, (&parents, &tracked), &mut links, purpose);
        let truncated = scanner.scan(directory)?;
        let Scan {
            entries: discovered,
            invalid_locations,
            directories,
            name_conflicts,
            ..
        } = scanner;
        warnings.extend(invalid_locations);
        entries.extend(discovered);
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(DirectoryScan {
            status: WorkspaceStatus {
                entries,
                bundle_warnings: warnings.into_iter().collect(),
                symlink_warnings: self.link_warnings(&links),
                truncated,
            },
            empty_directories: directories,
            name_conflicts,
        })
    }

    /// Lists the current directory in a local bundle, or workspace changes in an ordinary directory.
    /// # Errors
    /// Returns an error if the path is invalid or the directory, index, or changes cannot be read.
    pub fn browse(&self, folder: &Path) -> Result<WorkspacePage> {
        let directory = self.layout.local(folder)?;
        if let Some(bundle) = self.browse_bundle(&directory)? {
            return Ok(WorkspacePage::Bundle(bundle));
        }
        self.changes(&directory).map(WorkspacePage::Changes)
    }

    fn changes(&self, directory: &LocalPath) -> Result<WorkspaceChanges> {
        let track_paths = self.db.track_paths_in(directory)?;
        let checkouts = self.db.all_checkouts()?;
        let paths = track_paths
            .iter()
            .chain(checkouts.iter().map(|entry| &entry.path));
        let (parents, tracked) = tracked_paths(paths.clone());
        let mut entries: BTreeMap<_, _> = paths
            .filter_map(|path| {
                let child = direct_child(directory, path)?;
                (&child != path).then(|| (child.key(), directory_entry(child, false)))
            })
            .collect();

        let key = directory.key();
        let at_level = |path: &LocalPath| {
            (directory.is_root() && path.is_root())
                || path.parent().is_some_and(|parent| parent.key() == key)
        };
        let mut tracks = Vec::new();
        for path in track_paths.iter().filter(|path| at_level(path)) {
            tracks.extend(self.db.track(path)?);
        }
        let mut warnings: BTreeSet<_> = self
            .bundle_warnings_for(&tracks, &checkouts)?
            .into_iter()
            .filter(|path| contains(directory, path))
            .collect();
        let (tracked_status, mut links) =
            inspect::status(&self.layout, &tracks, checkouts, (directory, false))?;
        let mut scan = Scan::new(
            &self.layout,
            (&parents, &tracked),
            &mut links,
            ScanPurpose::Browse,
        );
        scan.scan(directory)?;
        for child in scan.directories {
            let can_submit = child.depth() > 1
                || read_directory(&self.layout.path(&child)?)?
                    .is_some_and(|mut children| children.next().is_some());
            entries.insert(child.key(), directory_entry(child, can_submit));
        }
        for status in tracked_status
            .into_iter()
            .filter(|status| at_level(&status.path) && visible(status))
            .chain(scan.entries)
        {
            entries.insert(status.path.key(), self.entry(status)?);
        }
        warnings.extend(scan.invalid_locations);
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_unstable_by(|left, right| {
            let directory = |entry: &WorkspaceEntry| entry.local_kind != Some(EntryKind::Directory);
            (directory(left), &left.status.path).cmp(&(directory(right), &right.status.path))
        });
        Ok(WorkspaceChanges {
            entries,
            bundle_warnings: warnings.into_iter().collect(),
            symlink_warnings: self.link_warnings(&links),
            truncated: false,
        })
    }

    fn link_warnings(&self, links: &[std::path::PathBuf]) -> Vec<String> {
        links
            .iter()
            .map(|path| bundle::relative_display(&self.layout.root, path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn entry(&self, status: Status) -> Result<WorkspaceEntry> {
        let local_kind = kind(&self.layout.path(&status.path)?)?;
        let can_submit = !status.remote_missing && !matches!(status.state, FileState::Clean);
        Ok(WorkspaceEntry {
            status,
            local_kind,
            can_submit,
        })
    }
}

fn directory_entry(child: LocalPath, can_submit: bool) -> WorkspaceEntry {
    WorkspaceEntry {
        status: untracked(child, false),
        local_kind: Some(EntryKind::Directory),
        can_submit,
    }
}

fn visible(status: &Status) -> bool {
    status.bundle || status.remote_missing || !matches!(status.state, FileState::Clean)
}

pub(super) fn pending_commit(status: &Status) -> bool {
    matches!(status.state, FileState::Modified | FileState::Untracked)
}

pub(super) fn contains(directory: &LocalPath, path: &LocalPath) -> bool {
    directory.is_root() || directory.key() == path.key() || direct_child(directory, path).is_some()
}

fn tracked_paths<'a>(
    paths: impl Iterator<Item = &'a LocalPath>,
) -> (HashSet<String>, HashSet<String>) {
    let mut parents = HashSet::new();
    let mut tracked = HashSet::new();
    for path in paths {
        tracked.insert(path.key());
        parents.extend(std::iter::successors(path.parent(), LocalPath::parent).map(|p| p.key()));
    }
    (parents, tracked)
}

pub(super) fn has_manifest(directory: &Path) -> Result<bool> {
    let metadata = if_present(fs::symlink_metadata(directory.join(IGNORE_FILE)))?;
    Ok(metadata.is_some_and(|metadata| metadata.is_file()))
}

fn read_directory(path: &Path) -> Result<Option<fs::ReadDir>> {
    if_present(fs::read_dir(path))
}

fn if_present<T>(result: io::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn local_entry(
    directory: &LocalPath,
    name: &std::ffi::OsStr,
    file_type: fs::FileType,
) -> Option<DirectoryEntry> {
    let name = name.to_str()?;
    let kind = if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else {
        return None;
    };
    let path = directory.child(name).ok()?;
    Some(DirectoryEntry { path, kind })
}

fn direct_child(directory: &LocalPath, path: &LocalPath) -> Option<LocalPath> {
    let key = directory.key();
    let mut child = path.clone();
    while let Some(parent) = child.parent() {
        if parent.key() == key {
            return Some(child);
        }
        child = parent;
    }
    None
}

fn untracked(path: LocalPath, bundle: bool) -> Status {
    Status {
        path,
        container_id: None,
        version: None,
        bundle,
        state: FileState::Untracked,
        file_digest: None,
        remote_missing: false,
        restored_from: None,
        backup: None,
    }
}
