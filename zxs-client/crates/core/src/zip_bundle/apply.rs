use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

use serde::Deserialize;

use super::{archive::Bundle, rules::Rules};
use crate::{
    domain::{BundlePlan, CheckoutFile, EntryKind, Error, LocalPath, Member, Result, Snapshot},
    store::{
        digest,
        layout::{checked_join, kind, same_existing_path},
    },
};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkout {
    #[default]
    IfClean,
    Backup,
}

pub fn plan_retirement(root: &Path) -> Result<BundlePlan> {
    let members = super::rules::collect_backup(root)?;
    let expected = members.len();
    let mut files = BTreeMap::new();
    let mut directories = Vec::new();
    for Member { path, kind, .. } in members {
        match kind {
            EntryKind::Directory => directories.push(path),
            EntryKind::File => {
                let old = Some(digest::file(&checked_join(root, &path)?)?);
                files.insert(path, CheckoutFile { old, new: None });
            }
        }
    }
    if fs::read_dir(root)?.count() + entry_count(root, directories.iter())? != expected {
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
) -> Result<BundlePlan> {
    let prior = match previous {
        Some(Snapshot::Bundle(members)) => members.as_slice(),
        _ => &[],
    };
    let rules = Rules::from_file(path)?;
    rules.check_replacement(path, bundle.rules())?;
    let expanded = bundle.extract(stage)?;
    let removal = if kind(path)? == Some(EntryKind::Directory) {
        let incoming = bundle.rules();
        plan(path, prior, &expanded.members, (&rules, incoming), retry)?
    } else {
        Removal::default()
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
    (local, incoming): (&Rules, &Rules),
    retry: bool,
) -> Result<Removal> {
    let mut removal = Removal::default();
    let mut removable = HashMap::new();
    for member in old {
        let path = &member.path;
        let Some(actual) = kind(&checked_join(root, path)?)? else {
            continue;
        };
        let is_dir = actual == EntryKind::Directory;
        if local.ignores(path, is_dir)
            || incoming.ignores(path, is_dir)
            || (is_dir && next.get(path) == Some(&EntryKind::Directory))
        {
            continue;
        }
        removable.insert(path.key(), (path, actual));
        if is_dir {
            removal.directories.push(path.clone());
        } else {
            removal.files.push(path.clone());
        }
    }
    for (path, desired) in next {
        let target = checked_join(root, path)?;
        let previous = removable.get(&path.key()).copied();
        if let Some((removed, EntryKind::Directory)) = previous
            && !matches!(
                path.parent()
                    .and_then(|parent| removable.get(&parent.key())),
                Some((_, EntryKind::Directory))
            )
        {
            let dirs = removal
                .directories
                .iter()
                .filter(|dir| *dir == removed || dir.below(removed));
            let planned = removable
                .values()
                .filter(|(entry, _)| entry.below(removed))
                .count();
            if entry_count(root, dirs)? != planned {
                return Err(collision(&checked_join(root, removed)?));
            }
        }
        match (kind(&target)?, previous) {
            (None, _) => {}
            (Some(_), Some((removed, _))) => {
                if removed != path && !same_existing_path(&checked_join(root, removed)?, &target) {
                    return Err(collision(&target));
                }
            }
            (Some(EntryKind::Directory), None) if *desired == EntryKind::Directory => {}
            (Some(EntryKind::File), None) if retry && *desired == EntryKind::File => {
                removal.files.push(path.clone());
            }
            (Some(_), None) => return Err(collision(&target)),
        }
    }
    removal
        .directories
        .sort_unstable_by_key(|path| std::cmp::Reverse(path.depth()));
    Ok(removal)
}

fn entry_count<'a>(root: &Path, dirs: impl Iterator<Item = &'a LocalPath>) -> Result<usize> {
    dirs.map(|dir| Ok(fs::read_dir(checked_join(root, dir)?)?.count()))
        .sum()
}

fn collision(path: &Path) -> Error {
    Error::LocalConflict(format!("本地存在未纳入目录包的内容：{}", path.display()))
}
