use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

use unicode_normalization::is_nfc;

use super::{
    DirectoryEntry, EntryKind, IGNORE_FILE, LocalPath, MAX_UNTRACKED, Result, ScanPurpose, Status,
    has_manifest, local_entry, read_directory, untracked,
};
use crate::{domain::file_name_key, store::layout::Layout};

pub(super) fn search_files(
    physical: &Path,
    directory: &LocalPath,
    query: &str,
    entries: &mut Vec<DirectoryEntry>,
) -> Result<bool> {
    for child in read_directory(physical)?.into_iter().flatten() {
        let child = child?;
        let name = child.file_name();
        if name.eq_ignore_ascii_case(IGNORE_FILE) {
            continue;
        }
        let Some(entry) = local_entry(directory, &name, child.file_type()?) else {
            continue;
        };
        if entry.kind == EntryKind::Directory {
            if search_files(&child.path(), &entry.path, query, entries)? {
                return Ok(true);
            }
        } else if file_name_key(entry.path.name()).contains(query) {
            if entries.len() == MAX_UNTRACKED {
                return Ok(true);
            }
            entries.push(entry);
        }
    }
    Ok(false)
}

pub(super) struct Scan<'a> {
    layout: &'a Layout,
    parents: &'a HashSet<String>,
    tracked: &'a HashSet<String>,
    links: &'a mut Vec<PathBuf>,
    pub(super) entries: Vec<Status>,
    pub(super) invalid_locations: BTreeSet<LocalPath>,
    pub(super) directories: Vec<LocalPath>,
    pub(super) name_conflicts: HashSet<(LocalPath, String)>,
    purpose: ScanPurpose,
}

impl<'a> Scan<'a> {
    pub(super) fn new(
        layout: &'a Layout,
        (parents, tracked): (&'a HashSet<String>, &'a HashSet<String>),
        links: &'a mut Vec<PathBuf>,
        purpose: ScanPurpose,
    ) -> Self {
        Self {
            layout,
            parents,
            tracked,
            links,
            entries: Vec::new(),
            invalid_locations: BTreeSet::new(),
            directories: Vec::new(),
            name_conflicts: HashSet::new(),
            purpose,
        }
    }

    pub(super) fn scan(&mut self, root: &LocalPath) -> Result<bool> {
        if self.tracked.contains(&root.key()) {
            return Ok(false);
        }
        let physical = self.layout.path(root)?;
        self.is_new_bundle(root, &physical)?;
        self.scan_children(root, &physical)
    }

    fn scan_children(&mut self, directory: &LocalPath, physical: &Path) -> Result<bool> {
        let Some(children) = read_directory(physical)? else {
            return Ok(false);
        };
        let committing = matches!(self.purpose, ScanPurpose::Commit);
        let mut names = HashSet::new();
        let mut has_entries = false;
        for next in children {
            let next = next?;
            let name = next.file_name();
            if committing
                && let Some(name) = name.to_str()
                && let Some(key) = names.replace(file_name_key(name))
            {
                self.name_conflicts.insert((directory.clone(), key));
            }
            let file_type = next.file_type()?;
            if file_type.is_symlink() {
                if committing || self.links.len() < MAX_UNTRACKED {
                    self.links.push(next.path());
                }
                continue;
            }
            if name.eq_ignore_ascii_case(IGNORE_FILE) {
                continue;
            }
            let Some(DirectoryEntry { path, kind }) = local_entry(directory, &name, file_type)
            else {
                continue;
            };
            let actual = next.path();
            if !committing && name.to_str().is_some_and(|name| !is_nfc(name)) {
                self.layout.local(&actual)?;
            }
            has_entries = true;
            if self.tracked.contains(&path.key()) {
                continue;
            }
            let bundle = kind == EntryKind::Directory && self.is_new_bundle(&path, &actual)?;
            if kind == EntryKind::File || bundle {
                if self.full() {
                    return Ok(true);
                }
                self.entries.push(untracked(path, bundle));
            } else if matches!(self.purpose, ScanPurpose::Browse) {
                self.directories.push(path);
            } else if self.scan_children(&path, &actual)? {
                return Ok(true);
            }
        }
        if committing && !has_entries && directory.depth() > 1 {
            self.directories.push(directory.clone());
        }
        Ok(false)
    }

    fn full(&self) -> bool {
        matches!(self.purpose, ScanPurpose::Status) && self.entries.len() >= MAX_UNTRACKED
    }

    fn is_new_bundle(&mut self, path: &LocalPath, physical: &Path) -> Result<bool> {
        if !has_manifest(physical)? {
            return Ok(false);
        }
        if path.depth() < 2 || self.parents.contains(&path.key()) {
            self.invalid_locations.insert(path.clone());
            return Ok(false);
        }
        Ok(true)
    }
}
