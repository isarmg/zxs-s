use std::{fs, io::ErrorKind, path::Path};

use crate::{
    domain::{
        Blake3Hash, BundlePlan, ContentHashes, EntryKind, Error, Member, MemberHashes, Result,
        Snapshot, TxId,
    },
    store::{
        digest,
        layout::{checked_join, kind, publish_staged_file, stamp, sync_parent},
    },
};

pub fn apply_bundle(
    root: &Path,
    stage: &Path,
    slot: &TxId,
    plan: &BundlePlan,
) -> Result<(Snapshot, ContentHashes)> {
    for (member, change) in &plan.files {
        let target = checked_join(root, member)?;
        let hash = current_hash(&target)?;
        if hash != change.old && hash != change.new {
            return Err(edited(&target));
        }
        if let Some(next) = &change.new
            && hash.as_ref() != Some(next)
            && kind(&checked_join(stage, member)?)? != Some(EntryKind::File)
        {
            return Err(Error::LocalConflict(
                "检出暂存缺失，请保留工作区并检查 .zxs 数据".into(),
            ));
        }
    }
    for (member, change) in &plan.files {
        if change.new.is_none() {
            let target = checked_join(root, member)?;
            if let Some(hash) = current_hash(&target)? {
                if change.old.as_ref() != Some(&hash) {
                    return Err(edited(&target));
                }
                fs::remove_file(&target)?;
                sync_parent(&target)?;
            }
        }
    }
    for member in &plan.remove_directories {
        let directory = checked_join(root, member)?;
        if kind(&directory)? == Some(EntryKind::Directory) {
            match fs::remove_dir(&directory) {
                Ok(()) => sync_parent(&directory)?,
                Err(error) if error.kind() == ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    install_members(root, stage, slot, plan)
}

fn install_members(
    root: &Path,
    stage: &Path,
    slot: &TxId,
    plan: &BundlePlan,
) -> Result<(Snapshot, ContentHashes)> {
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
                let change = plan.files.get(member).ok_or_else(|| edited(&target))?;
                let next = change.new.as_ref().ok_or_else(|| edited(&target))?;
                let current = current_hash(&target)?;
                if current.as_ref() != Some(next) {
                    if current != change.old || kind(&target)? == Some(EntryKind::Directory) {
                        return Err(edited(&target));
                    }
                    publish_staged_file(&checked_join(stage, member)?, &target, slot, next)?;
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

fn current_hash(path: &Path) -> Result<Option<Blake3Hash>> {
    if kind(path)? == Some(EntryKind::File) {
        Ok(Some(digest::file(path)?))
    } else {
        Ok(None)
    }
}

fn edited(path: &Path) -> Error {
    Error::LocalConflict(format!("检出期间内容已改变，已保留：{}", path.display()))
}
