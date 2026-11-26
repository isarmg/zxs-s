use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use rayon::prelude::*;

use crate::{
    domain::{
        ContentHashes, EntryKind, Error, FileState, InterruptedCheckout, LocalPath, Result,
        Snapshot, Status, Track,
    },
    store::{
        digest,
        layout::{IGNORE_FILE, Layout, checked_join, kind, stamp},
    },
    zip_bundle,
};

pub(super) fn status(
    layout: &Layout,
    directory: &LocalPath,
    tracks: &[Track],
    checkouts: Vec<InterruptedCheckout>,
) -> Result<(Vec<Status>, Vec<PathBuf>)> {
    let interrupted: BTreeSet<_> = checkouts.iter().map(|entry| &entry.container_id).collect();
    let tracked: Vec<_> = tracks
        .par_iter()
        .filter(|track| !interrupted.contains(&track.container_id))
        .map(|track| status_one(layout, track))
        .collect();
    let mut statuses = BTreeMap::new();
    let mut links = Vec::new();
    for item in tracked {
        match item {
            Ok(status) => {
                statuses.insert(status.path.clone(), status);
            }
            Err(Error::SymbolicLink(path)) => links.push(path),
            Err(error) => return Err(error),
        }
    }
    for checkout in checkouts
        .into_iter()
        .filter(|entry| super::browse::contains(directory, &entry.path))
    {
        statuses.insert(
            checkout.path.clone(),
            Status {
                path: checkout.path,
                container_id: Some(checkout.container_id),
                version: Some(checkout.version),
                bundle: checkout.kind == EntryKind::Directory,
                state: FileState::Interrupted,
                remote_missing: false,
                restored_from: None,
                backup: checkout
                    .backup
                    .map(|path| layout.root.join(path))
                    .filter(|path| path.exists()),
            },
        );
    }
    Ok((statuses.into_values().collect(), links))
}

pub(super) fn status_one(layout: &Layout, track: &Track) -> Result<Status> {
    let target = layout.path(&track.path)?;
    let clean = match (&track.snapshot, kind(&target)?) {
        (_, None) => FileState::Missing,
        _ if track.restored_from.is_some() => FileState::Modified,
        (Snapshot::File(previous), Some(EntryKind::File)) if stamp(&target)? == *previous => {
            FileState::Clean
        }
        (Snapshot::Bundle(previous), Some(EntryKind::Directory))
            if kind(&target.join(IGNORE_FILE))? == Some(EntryKind::File)
                && zip_bundle::collect(&target)? == *previous =>
        {
            FileState::Clean
        }
        _ => FileState::Modified,
    };
    Ok(Status {
        path: track.path.clone(),
        container_id: Some(track.container_id.clone()),
        version: Some(track.version),
        bundle: matches!(track.snapshot, Snapshot::Bundle(_)),
        state: clean,
        remote_missing: track.remote_missing,
        restored_from: track.restored_from,
        backup: None,
    })
}

pub(super) fn same_contents(layout: &Layout, track: &Track) -> Result<bool> {
    let path = layout.path(&track.path)?;
    match &track.hashes {
        ContentHashes::File(hash) => Ok(digest::file(&path)? == *hash),
        ContentHashes::Bundle(hashes) => {
            for (member, hash) in hashes {
                if digest::file(&checked_join(&path, member)?)? != *hash {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}
