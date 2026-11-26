use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::ErrorKind,
    path::Path,
};

use serde::Serialize;

use super::{EntryKind, FileState, LocalPath, Result, Status, Workspace, inspect, kind};
use crate::store::layout::{IGNORE_FILE, WORKSPACE_DIR};

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
            .map(|(status, _)| status)
    }

    pub(super) fn scan_directory(
        &self,
        directory: &LocalPath,
        purpose: ScanPurpose,
    ) -> Result<(WorkspaceStatus, Vec<LocalPath>)> {
        let tracks = if directory.is_root() {
            self.db.all_tracks()?
        } else {
            self.db.tracks_in(directory)?
        };
        let checkouts = self.db.all_checkouts()?;
        let mut warnings: BTreeSet<_> = self
            .bundle_warnings_for(&tracks, &checkouts)?
            .into_iter()
            .filter(|path| contains(directory, path))
            .collect();
        let scope = (directory, matches!(purpose, ScanPurpose::Commit));
        let (mut entries, mut links) = inspect::status(&self.layout, &tracks, checkouts, scope)?;
        let (parents, tracked) = tracked_paths(&entries);
        let mut scanner = Scan::new(&self.layout, (&parents, &tracked), &mut links, purpose);
        let truncated = scanner.scan(directory)?;
        let Scan {
            entries: discovered,
            invalid_locations,
            empty_directories,
            ..
        } = scanner;
        warnings.extend(invalid_locations);
        entries.extend(discovered);
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok((
            WorkspaceStatus {
                entries,
                bundle_warnings: warnings.into_iter().collect(),
                symlink_warnings: links
                    .iter()
                    .map(|path| bundle::relative_display(&self.layout.root, path))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                truncated,
            },
            empty_directories,
        ))
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
        let (scan, empty) = self.scan_directory(directory, ScanPurpose::Browse)?;
        let mut entries = BTreeMap::new();
        for status in scan.entries.into_iter().filter(visible) {
            if directory.is_root() && status.path.is_root() {
                entries.insert(status.path.clone(), self.entry(status)?);
                continue;
            }
            let Some(child) = direct_child(directory, &status.path) else {
                continue;
            };
            if child == status.path {
                entries.insert(child, self.entry(status)?);
            } else {
                add_directory(&mut entries, child, pending_commit(&status));
            }
        }
        for path in empty {
            if let Some(child) = direct_child(directory, &path) {
                add_directory(&mut entries, child, path.depth() > 1);
            }
        }
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_by_key(|entry| entry.local_kind != Some(EntryKind::Directory));
        Ok(WorkspaceChanges {
            entries,
            bundle_warnings: scan.bundle_warnings,
            symlink_warnings: scan.symlink_warnings,
            truncated: scan.truncated,
        })
    }

    fn entry(&self, status: Status) -> Result<WorkspaceEntry> {
        let local_kind = kind(&self.layout.path(&status.path)?)?;
        let can_submit = !status.remote_missing;
        Ok(WorkspaceEntry {
            status,
            local_kind,
            can_submit,
        })
    }
}

fn add_directory(
    entries: &mut BTreeMap<LocalPath, WorkspaceEntry>,
    child: LocalPath,
    can_submit: bool,
) {
    entries
        .entry(child.clone())
        .and_modify(|entry| entry.can_submit |= can_submit)
        .or_insert_with(|| WorkspaceEntry {
            status: untracked(child, false),
            local_kind: Some(EntryKind::Directory),
            can_submit,
        });
}

fn visible(status: &Status) -> bool {
    status.remote_missing || !matches!(status.state, FileState::Clean)
}

pub(super) fn pending_commit(status: &Status) -> bool {
    !status.remote_missing && matches!(status.state, FileState::Modified | FileState::Untracked)
}

pub(super) fn contains(directory: &LocalPath, path: &LocalPath) -> bool {
    directory.is_root() || directory.key() == path.key() || direct_child(directory, path).is_some()
}

fn tracked_paths(entries: &[Status]) -> (HashSet<String>, HashSet<String>) {
    let mut parents = HashSet::new();
    let mut tracked = HashSet::new();
    for entry in entries {
        tracked.insert(entry.path.as_str().to_lowercase());
        let mut parent = entry.path.parent();
        while let Some(path) = parent {
            parents.insert(path.as_str().to_lowercase());
            parent = path.parent();
        }
    }
    (parents, tracked)
}

fn has_manifest(directory: &Path) -> Result<bool> {
    match fs::symlink_metadata(directory.join(IGNORE_FILE)) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_directory(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(children) => Ok(Some(children)),
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
    if name.eq_ignore_ascii_case(WORKSPACE_DIR) {
        return None;
    }
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
    let key = directory.as_str().to_lowercase();
    let mut child = path.clone();
    while let Some(parent) = child.parent() {
        if parent.as_str().to_lowercase() == key {
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
