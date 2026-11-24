use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::{archive::Bundle, rules::Rules};
use crate::{
    domain::{EntryKind, Error, LocalPath, Member, Result, Snapshot},
    store::layout::{TempDir, checked_join, kind, parent},
};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkout {
    #[default]
    IfClean,
    Backup,
}

enum Undo {
    RestoreFile(PathBuf, PathBuf),
    RemoveFile(PathBuf),
    RestoreDir(PathBuf),
    RemoveDir(PathBuf),
}

pub struct Apply {
    temp: TempDir,
    undo: Vec<Undo>,
    committed: bool,
}

impl Apply {
    pub fn new(temp: TempDir) -> Self {
        Self {
            temp,
            undo: Vec::new(),
            committed: false,
        }
    }
    pub fn payload(&self) -> PathBuf {
        self.temp.child("payload")
    }
    pub fn commit(mut self) {
        self.committed = true;
    }

    pub fn install_file(&mut self, destination: &Path, tracked: bool) -> Result<Snapshot> {
        let source = self.payload();
        match kind(destination)? {
            Some(EntryKind::File) if tracked => self.remove_file(destination)?,
            Some(_) => return Err(collision(destination)),
            None => {}
        }
        self.ensure_directory(parent(destination)?)?;
        self.place(&source, destination)?;
        Ok(Snapshot::File(crate::store::layout::stamp(destination)?))
    }

    pub fn install_bundle(
        &mut self,
        path: &Path,
        bundle: &mut Bundle,
        previous: Option<&Snapshot>,
    ) -> Result<Snapshot> {
        let old = match previous {
            Some(Snapshot::Bundle(members)) => members.as_slice(),
            _ => &[],
        };
        let rules = if kind(path)? == Some(EntryKind::Directory) {
            Rules::from_file(path)?
        } else {
            Rules::from_bytes(&[])?
        };
        let stage = self.temp.child("expanded");
        let next = bundle.extract(&stage, &rules)?;
        let plan = plan(path, old, &next, &rules, bundle.rules())?;
        if kind(path)? == Some(EntryKind::File) {
            if !matches!(previous, Some(Snapshot::File(_))) {
                return Err(collision(path));
            }
            self.remove_file(path)?;
        }
        self.ensure_directory(path)?;
        self.apply_plan(path, &stage, &next, plan)
    }

    fn apply_plan(
        &mut self,
        root: &Path,
        stage: &Path,
        next: &BTreeMap<LocalPath, EntryKind>,
        plan: Removal,
    ) -> Result<Snapshot> {
        for file in plan.files {
            self.remove_file(&checked_join(root, &file)?)?;
        }
        for dir in plan.directories {
            let path = checked_join(root, &dir)?;
            match fs::remove_dir(&path) {
                Ok(()) => self.undo.push(Undo::RestoreDir(path)),
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut members = Vec::new();
        for (local, entry) in next {
            let target = checked_join(root, local)?;
            let stamp = match entry {
                EntryKind::Directory => {
                    self.ensure_directory(&target)?;
                    None
                }
                EntryKind::File => {
                    if kind(&target)?.is_some() {
                        return Err(collision(&target));
                    }
                    self.place(&checked_join(stage, local)?, &target)?;
                    Some(crate::store::layout::stamp(&target)?)
                }
            };
            members.push(Member {
                path: local.clone(),
                kind: *entry,
                stamp,
            });
        }
        Ok(Snapshot::Bundle(members))
    }

    fn ensure_directory(&mut self, path: &Path) -> Result<()> {
        match kind(path)? {
            Some(EntryKind::Directory) => return Ok(()),
            Some(_) => return Err(collision(path)),
            None => {}
        }
        self.ensure_directory(parent(path)?)?;
        fs::create_dir(path)?;
        self.undo.push(Undo::RemoveDir(path.into()));
        Ok(())
    }

    fn remove_file(&mut self, original: &Path) -> Result<()> {
        let backup = self.temp.child(&format!("previous-{}", self.undo.len()));
        fs::rename(original, &backup)?;
        self.undo.push(Undo::RestoreFile(backup, original.into()));
        Ok(())
    }

    fn place(&mut self, source: &Path, target: &Path) -> Result<()> {
        // A pre-existing target is moved into this transaction before publication.
        fs::rename(source, target)?;
        self.undo.push(Undo::RemoveFile(target.into()));
        Ok(())
    }
}

impl Drop for Apply {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for step in self.undo.drain(..).rev() {
            let result = match step {
                Undo::RestoreFile(backup, target) => fs::rename(backup, target),
                Undo::RemoveFile(target) => fs::remove_file(target),
                Undo::RestoreDir(path) => fs::create_dir(path),
                Undo::RemoveDir(path) => fs::remove_dir(path),
            };
            if let Err(error) = result {
                self.temp.preserve();
                eprintln!(
                    "could not roll back checkout in {}: {error}",
                    self.temp.child(".").display()
                );
            }
        }
    }
}

struct Removal {
    files: Vec<LocalPath>,
    directories: Vec<LocalPath>,
}

fn plan(
    root: &Path,
    old: &[Member],
    next: &BTreeMap<LocalPath, EntryKind>,
    local: &Rules,
    incoming: &Rules,
) -> Result<Removal> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut removable = HashSet::new();
    for member in old {
        let path = &member.path;
        let is_dir = member.kind == EntryKind::Directory;
        if local.ignores(path, is_dir) || incoming.ignores(path, is_dir) {
            continue;
        }
        match kind(&checked_join(root, path)?)? {
            Some(EntryKind::File) => {
                files.push(path.clone());
                removable.insert(path);
            }
            Some(EntryKind::Directory) if next.get(path) != Some(&EntryKind::Directory) => {
                directories.push(path.clone());
                removable.insert(path);
            }
            _ => {}
        }
    }
    for (path, desired) in next {
        let target = checked_join(root, path)?;
        match kind(&target)? {
            None => {}
            Some(EntryKind::Directory) if *desired == EntryKind::Directory => {}
            Some(_) if removable.contains(path) => {}
            Some(_) => return Err(collision(&target)),
        }
    }
    directories.sort_unstable_by_key(|path| std::cmp::Reverse(path.depth()));
    Ok(Removal { files, directories })
}

fn collision(path: &Path) -> Error {
    Error::LocalConflict(format!(
        "checkout would overwrite an untracked entry: {}",
        path.display()
    ))
}
