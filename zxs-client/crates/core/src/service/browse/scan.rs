use std::{
    collections::{BTreeSet, HashSet},
    fs::ReadDir,
    path::PathBuf,
};

use unicode_normalization::is_nfc;

use super::{
    DirectoryEntry, EntryKind, IGNORE_FILE, LocalPath, MAX_UNTRACKED, Result, ScanPurpose, Status,
    has_manifest, local_entry, read_directory, untracked,
};
use crate::{domain::file_name_key, store::layout::Layout};

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

struct DirectoryEntries {
    path: LocalPath,
    children: ReadDir,
    has_entries: bool,
    names: HashSet<String>,
}

impl DirectoryEntries {
    fn new(path: LocalPath, children: ReadDir) -> Self {
        Self {
            path,
            children,
            has_entries: false,
            names: HashSet::new(),
        }
    }
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
        self.is_new_bundle(root)?;
        let Some(children) = read_directory(&self.layout.path(root)?)? else {
            return Ok(false);
        };
        let mut active = vec![DirectoryEntries::new(root.clone(), children)];
        while let Some(directory) = active.last_mut() {
            let Some(next) = directory.children.next() else {
                if matches!(self.purpose, ScanPurpose::Commit)
                    && !directory.has_entries
                    && directory.path.depth() > 1
                {
                    self.directories.push(directory.path.clone());
                }
                active.pop();
                continue;
            };
            let next = next?;
            let name = next.file_name();
            if matches!(self.purpose, ScanPurpose::Commit)
                && let Some(name) = name.to_str()
                && let Some(key) = directory.names.replace(file_name_key(name))
            {
                self.name_conflicts.insert((directory.path.clone(), key));
            }
            let file_type = next.file_type()?;
            if file_type.is_symlink() {
                if matches!(self.purpose, ScanPurpose::Commit) || self.links.len() < MAX_UNTRACKED {
                    self.links.push(next.path());
                }
                continue;
            }
            if name.eq_ignore_ascii_case(IGNORE_FILE) {
                continue;
            }
            let Some(DirectoryEntry { path, kind }) =
                local_entry(&directory.path, &name, file_type)
            else {
                continue;
            };
            if name.to_str().is_some_and(|name| !is_nfc(name)) {
                self.layout.local(&next.path())?;
            }
            directory.has_entries = true;
            if self.tracked.contains(&path.key()) {
                continue;
            }
            let bundle = kind == EntryKind::Directory && self.is_new_bundle(&path)?;
            if kind == EntryKind::File || bundle {
                if self.full() {
                    return Ok(true);
                }
                self.entries.push(untracked(path, bundle));
            } else if matches!(self.purpose, ScanPurpose::Browse) {
                self.directories.push(path);
            } else if let Some(children) = read_directory(&self.layout.path(&path)?)? {
                active.push(DirectoryEntries::new(path, children));
            }
        }
        Ok(false)
    }

    fn full(&self) -> bool {
        matches!(self.purpose, ScanPurpose::Status) && self.entries.len() >= MAX_UNTRACKED
    }

    fn is_new_bundle(&mut self, path: &LocalPath) -> Result<bool> {
        if !has_manifest(&self.layout.path(path)?)? {
            return Ok(false);
        }
        if path.depth() < 2 || self.parents.contains(&path.key()) {
            self.invalid_locations.insert(path.clone());
            return Ok(false);
        }
        Ok(true)
    }
}
