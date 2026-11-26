use std::{fs, io::ErrorKind, path::Path};

use crate::{
    domain::{
        BundlePlan, ContentHashes, EntryKind, Error, Member, MemberHashes, Result, Snapshot, TxId,
    },
    store::{
        digest,
        layout::{checked_join, kind, move_new, publish_staged_file, stamp, sync_parent},
    },
};

#[derive(Clone, Copy)]
pub struct BundleInstall<'a> {
    pub stage: &'a Path,
    pub slot: &'a TxId,
    pub backup: Option<&'a Path>,
}

pub fn apply_bundle(
    root: &Path,
    plan: &BundlePlan,
    install: BundleInstall<'_>,
) -> Result<(Snapshot, ContentHashes)> {
    retain_members(root, plan, install.backup)?;
    fs::create_dir_all(root)?;
    let mut members = Vec::new();
    let mut hashes = MemberHashes::new();
    for (member, entry) in &plan.members {
        let target = checked_join(root, member)?;
        let stamp = match entry {
            EntryKind::Directory => {
                fs::create_dir_all(&target)?;
                None
            }
            EntryKind::File => {
                let next = plan
                    .files
                    .get(member)
                    .and_then(|change| change.new.as_ref())
                    .ok_or_else(|| edited(&target))?;
                match kind(&target)? {
                    None => publish_staged_file(
                        &checked_join(install.stage, member)?,
                        &target,
                        install.slot,
                        next,
                    )?,
                    Some(EntryKind::File) if digest::file(&target)? == *next => {}
                    Some(_) => return Err(edited(&target)),
                }
                hashes.insert(member.clone(), next.clone());
                Some(stamp(&target)?)
            }
        };
        members.push(Member {
            path: member.clone(),
            kind: *entry,
            stamp,
        });
    }
    Ok((Snapshot::Bundle(members), ContentHashes::Bundle(hashes)))
}

pub fn retain_members(root: &Path, plan: &BundlePlan, backup: Option<&Path>) -> Result<()> {
    for (member, change) in &plan.files {
        if change.old.is_none() || change.old == change.new {
            continue;
        }
        let target = checked_join(root, member)?;
        if kind(&target)? != Some(EntryKind::File)
            || change.old.as_ref() != Some(&digest::file(&target)?)
        {
            return Err(edited(&target));
        }
        let backup = backup.ok_or_else(|| edited(&target))?;
        move_new(&target, &checked_join(backup, member)?)?;
    }
    for member in &plan.remove_directories {
        remove_empty(&checked_join(root, member)?)?;
    }
    Ok(())
}

pub fn remove_empty(directory: &Path) -> Result<()> {
    if kind(directory)? == Some(EntryKind::Directory) {
        match fs::remove_dir(directory) {
            Ok(()) => sync_parent(directory)?,
            Err(error) if error.kind() == ErrorKind::DirectoryNotEmpty => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn edited(path: &Path) -> Error {
    Error::LocalConflict(format!("检出期间内容已改变，已保留：{}", path.display()))
}
