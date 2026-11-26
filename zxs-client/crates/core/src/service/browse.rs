use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::Serialize;
use unicode_normalization::is_nfc;

use super::{EntryKind, FileState, LocalPath, Result, Status, Workspace, inspect, kind};
use crate::store::layout::{IGNORE_FILE, Layout, WORKSPACE_DIR};

mod bundle;

const MAX_UNTRACKED: usize = 1000;

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
        self.scan_directory(&LocalPath::parse(".")?, Some(MAX_UNTRACKED))
            .map(|(status, _)| status)
    }

    pub(super) fn scan_directory(
        &self,
        directory: &LocalPath,
        limit: Option<usize>,
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
        let (mut entries, mut links) =
            inspect::status(&self.layout, directory, &tracks, checkouts)?;
        let (parents, tracked) = tracked_paths(&entries);
        let mut scanner = Scan {
            layout: &self.layout,
            parents: &parents,
            tracked: &tracked,
            entries: Vec::new(),
            links: &mut links,
            invalid_locations: BTreeSet::new(),
            empty_directories: Vec::new(),
            limit,
        };
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
        let scan = self.status()?;
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
                let can_submit = pending_commit(&status);
                entries
                    .entry(child.clone())
                    .and_modify(|entry| entry.can_submit |= can_submit)
                    .or_insert_with(|| WorkspaceEntry {
                        status: untracked(child, false),
                        local_kind: Some(EntryKind::Directory),
                        can_submit,
                    });
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

struct Scan<'a> {
    layout: &'a Layout,
    parents: &'a HashSet<String>,
    tracked: &'a HashSet<String>,
    entries: Vec<Status>,
    links: &'a mut Vec<PathBuf>,
    invalid_locations: BTreeSet<LocalPath>,
    empty_directories: Vec<LocalPath>,
    limit: Option<usize>,
}

impl Scan<'_> {
    fn scan(&mut self, root: &LocalPath) -> Result<bool> {
        if self.tracked.contains(&root.key()) {
            return Ok(false);
        }
        self.is_new_bundle(root)?;
        let Some(children) = read_directory(&self.layout.path(root)?)? else {
            return Ok(false);
        };
        let mut active = vec![(root.clone(), children, false)];
        while let Some((directory, children, has_entries)) = active.last_mut() {
            let Some(next) = children.next() else {
                if self.limit.is_none() && !*has_entries && directory.depth() > 1 {
                    self.empty_directories.push(directory.clone());
                }
                active.pop();
                continue;
            };
            let next = next?;
            let file_type = next.file_type()?;
            if file_type.is_symlink() {
                if self.limit.is_none_or(|limit| self.links.len() < limit) {
                    self.links.push(next.path());
                }
                continue;
            }
            let name = next.file_name();
            if name.eq_ignore_ascii_case(IGNORE_FILE) {
                continue;
            }
            let Some(DirectoryEntry { path, kind }) = local_entry(directory, &name, file_type)
            else {
                continue;
            };
            if name.to_str().is_some_and(|name| !is_nfc(name)) {
                self.layout.local(&next.path())?;
            }
            *has_entries = true;
            if self.tracked.contains(&path.as_str().to_lowercase()) {
                continue;
            }
            let bundle = kind == EntryKind::Directory && self.is_new_bundle(&path)?;
            if kind == EntryKind::File || bundle {
                if self.limit == Some(self.entries.len()) {
                    return Ok(true);
                }
                self.entries.push(untracked(path, bundle));
            } else if let Some(children) = read_directory(&self.layout.path(&path)?)? {
                active.push((path, children, false));
            }
        }
        Ok(false)
    }

    fn is_new_bundle(&mut self, path: &LocalPath) -> Result<bool> {
        if self.parents.contains(&path.key()) || !has_manifest(&self.layout.path(path)?)? {
            return Ok(false);
        }
        if path.depth() < 2 {
            self.invalid_locations.insert(path.clone());
            return Ok(false);
        }
        Ok(true)
    }
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
        remote_missing: false,
        restored_from: None,
        backup: None,
    }
}
