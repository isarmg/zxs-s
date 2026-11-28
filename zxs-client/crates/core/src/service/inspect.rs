use std::{collections::BTreeSet, path::PathBuf};

use rayon::prelude::*;

use crate::{
    domain::{
        ContentHashes, EntryKind, Error, FileState, InterruptedCheckout, LocalPath, Result,
        Snapshot, Status, Track,
    },
    store::{
        digest,
        layout::{Layout, checked_join, kind, stamp},
    },
    zip_bundle,
};

pub(super) fn status(
    layout: &Layout,
    tracks: &[Track],
    checkouts: Vec<InterruptedCheckout>,
    (directory, verify_contents): (&LocalPath, bool),
) -> Result<(Vec<Status>, Vec<PathBuf>)> {
    let interrupted: BTreeSet<_> = checkouts.iter().map(|entry| &entry.container_id).collect();
    let tracked: Vec<_> = tracks
        .par_iter()
        .filter(|track| !interrupted.contains(&track.container_id))
        .map(|track| {
            let state = inspect_track(layout, track, true)?;
            let mut status = Status {
                path: track.path.clone(),
                container_id: Some(track.container_id.clone()),
                version: Some(track.version),
                bundle: matches!(track.snapshot, Snapshot::Bundle(_)),
                state,
                file_digest: None,
                remote_missing: track.remote_missing,
                restored_from: track.restored_from,
                backup: None,
            };
            if verify_contents && matches!(status.state, FileState::Clean) {
                check_contents(layout, track, &mut status)?;
            }
            Ok(status)
        })
        .collect();
    let mut statuses = Vec::new();
    let mut links = Vec::new();
    for item in tracked {
        match item {
            Ok(status) => statuses.push(status),
            Err(Error::SymbolicLink(path)) => links.push(path),
            Err(error) => return Err(error),
        }
    }
    for checkout in checkouts
        .into_iter()
        .filter(|entry| super::browse::contains(directory, &entry.path))
    {
        statuses.push(Status {
            path: checkout.path,
            container_id: Some(checkout.container_id),
            version: Some(checkout.version),
            bundle: checkout.kind == EntryKind::Directory,
            state: FileState::Interrupted,
            file_digest: None,
            remote_missing: false,
            restored_from: None,
            backup: checkout
                .backup
                .map(|path| layout.root.join(path))
                .filter(|path| path.exists()),
        });
    }
    Ok((statuses, links))
}

fn check_contents(layout: &Layout, track: &Track, status: &mut Status) -> Result<()> {
    if let (Snapshot::File(stamp), ContentHashes::File(expected)) = (&track.snapshot, &track.hashes)
    {
        let actual = digest::file(&layout.path(&track.path)?)?;
        if &actual != expected {
            status.state = FileState::Modified;
            status.file_digest = Some((stamp.clone(), actual));
        }
    } else if !same_contents(layout, track)? {
        status.state = FileState::Modified;
    }
    Ok(())
}

pub(super) fn local_state(layout: &Layout, track: &Track) -> Result<FileState> {
    inspect_track(layout, track, false)
}

fn inspect_track(layout: &Layout, track: &Track, invalid_as_modified: bool) -> Result<FileState> {
    let target = layout.path(&track.path)?;
    Ok(match (&track.snapshot, kind(&target)?) {
        (_, None) => FileState::Missing,
        _ if track.restored_from.is_some() => FileState::Modified,
        (Snapshot::File(previous), Some(EntryKind::File)) if stamp(&target)? == *previous => {
            FileState::Clean
        }
        (Snapshot::Bundle(previous), Some(EntryKind::Directory))
            if let Some(manifest) = zip_bundle::manifest_path(&target)?
                && match zip_bundle::collect(&target, &manifest) {
                    Err(Error::Invalid(_)) if invalid_as_modified => false,
                    members => members? == *previous,
                } =>
        {
            FileState::Clean
        }
        _ => FileState::Modified,
    })
}

pub(super) fn clean_contents(layout: &Layout, track: &Track) -> Result<bool> {
    Ok(matches!(local_state(layout, track)?, FileState::Clean) && same_contents(layout, track)?)
}

fn same_contents(layout: &Layout, track: &Track) -> Result<bool> {
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
