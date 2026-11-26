use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::Path,
};

use serde::Deserialize;

use super::{archive::Bundle, rules::Rules};
use crate::{
    domain::{BundlePlan, CheckoutFile, EntryKind, Error, LocalPath, Member, Result, Snapshot},
    store::{
        digest,
        layout::{checked_join, kind},
    },
};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkout {
    #[default]
    IfClean,
    Backup,
}

pub fn plan_retirement(root: &Path, needs_empty_root: bool) -> Result<BundlePlan> {
    let members = super::rules::collect_backup(root)?;
    let mut files = BTreeMap::new();
    let mut directories = Vec::new();
    let mut entries = 0;
    for member in &members {
        let path = checked_join(root, &member.path)?;
        match member.kind {
            EntryKind::Directory => {
                directories.push(member.path.clone());
                if needs_empty_root {
                    entries += fs::read_dir(&path)?.count();
                }
            }
            EntryKind::File => {
                files.insert(
                    member.path.clone(),
                    CheckoutFile {
                        old: Some(digest::file(&path)?),
                        new: None,
                    },
                );
            }
        }
    }
    if needs_empty_root && entries + fs::read_dir(root)?.count() != members.len() {
        return Err(collision(root));
    }
    directories.sort_unstable_by_key(|path| std::cmp::Reverse(path.depth()));
    Ok(BundlePlan {
        files,
        members: BTreeMap::new(),
        remove_directories: directories,
    })
}

pub fn plan_bundle(
    path: &Path,
    stage: &Path,
    bundle: &mut Bundle,
    previous: Option<&Snapshot>,
    retry: bool,
    replace_target: bool,
) -> Result<BundlePlan> {
    let prior = match previous {
        Some(Snapshot::Bundle(members)) => members.as_slice(),
        _ => &[],
    };
    let actual = if replace_target { None } else { kind(path)? };
    let rules = match actual {
        Some(EntryKind::File) => return Err(collision(path)),
        Some(EntryKind::Directory) => Rules::from_file(path)?,
        None => Rules::from_bytes(&[])?,
    };
    let expanded = bundle.extract(stage, &rules, path)?;
    let removal = if replace_target {
        Removal::default()
    } else {
        plan(
            path,
            prior,
            &expanded.members,
            &rules,
            bundle.rules(),
            retry,
        )?
    };
    let mut files = BTreeMap::<LocalPath, CheckoutFile>::new();
    for member in removal.files {
        let old = digest::file(&checked_join(path, &member)?)?;
        files.entry(member).or_default().old = Some(old);
    }
    for (member, hash) in expanded.hashes {
        files.entry(member).or_default().new = Some(hash);
    }
    Ok(BundlePlan {
        files,
        members: expanded.members,
        remove_directories: removal.directories,
    })
}

#[derive(Default)]
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
    retry: bool,
) -> Result<Removal> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut removable = HashSet::new();
    for member in old {
        let path = &member.path;
        let Some(actual) = kind(&checked_join(root, path)?)? else {
            continue;
        };
        let is_dir = actual == EntryKind::Directory;
        if local.ignores(path, is_dir) || incoming.ignores(path, is_dir) {
            continue;
        }
        match actual {
            EntryKind::File => {
                files.push(path.clone());
                removable.insert(path);
            }
            EntryKind::Directory if next.get(path) != Some(&EntryKind::Directory) => {
                directories.push(path.clone());
                removable.insert(path);
            }
            EntryKind::Directory => {}
        }
    }
    for (path, desired) in next {
        let target = checked_join(root, path)?;
        match kind(&target)? {
            None => {}
            Some(EntryKind::Directory) if *desired == EntryKind::Directory => {}
            Some(_) if removable.contains(path) => {}
            Some(EntryKind::File) if retry && *desired == EntryKind::File => {
                files.push(path.clone());
            }
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
