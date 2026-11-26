use std::{
    collections::{BTreeSet, HashSet},
    path::PathBuf,
};

use unicode_normalization::is_nfc;

use super::{
    DirectoryEntry, EntryKind, IGNORE_FILE, LocalPath, MAX_UNTRACKED, Result, ScanPurpose, Status,
    has_manifest, local_entry, read_directory, untracked,
};
use crate::store::layout::Layout;

pub(super) struct Scan<'a> {
    layout: &'a Layout,
    parents: &'a HashSet<String>,
    tracked: &'a HashSet<String>,
    links: &'a mut Vec<PathBuf>,
    pub(super) entries: Vec<Status>,
    pub(super) invalid_locations: BTreeSet<LocalPath>,
    pub(super) directories: Vec<LocalPath>,
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
        let mut active = vec![(root.clone(), children, false)];
        while let Some((directory, children, has_entries)) = active.last_mut() {
            let Some(next) = children.next() else {
                if matches!(self.purpose, ScanPurpose::Commit)
                    && !*has_entries
                    && directory.depth() > 1
                {
                    self.directories.push(directory.clone());
                }
                active.pop();
                continue;
            };
            let next = next?;
            let file_type = next.file_type()?;
            if file_type.is_symlink() {
                if matches!(self.purpose, ScanPurpose::Commit) || self.links.len() < MAX_UNTRACKED {
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
                active.push((path, children, false));
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
