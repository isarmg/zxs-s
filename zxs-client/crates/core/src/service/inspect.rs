use std::{collections::BTreeMap, path::PathBuf};

use rayon::prelude::*;

use crate::{
    domain::{ContentHashes, EntryKind, Error, FileState, Result, Snapshot, Status, Track},
    store::{
        digest,
        layout::{IGNORE_FILE, Layout, checked_join, kind, stamp},
    },
    zip_bundle,
};

pub(super) fn status(layout: &Layout, tracks: &[Track]) -> Result<(Vec<Status>, Vec<PathBuf>)> {
    let tracked: Vec<_> = tracks
        .par_iter()
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
    Ok((statuses.into_values().collect(), links))
}

pub(super) fn status_one(layout: &Layout, track: &Track) -> Result<Status> {
    let target = layout.path(&track.path)?;
    let clean = match (&track.snapshot, kind(&target)?) {
        (_, None) => FileState::Missing,
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
