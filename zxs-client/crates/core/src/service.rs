mod checkout;
mod commit;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use rayon::prelude::*;

use crate::{
    domain::{
        Commit, Container, ContainerId, EntryKind, Error, FileName, FileState, FolderPath,
        LocalPath, NewContainer, PendingCommit, Result, Snapshot, Status, Track, VersionNum,
        invalid, normalize_tags,
    },
    store::{
        db::Db,
        layout::{Layout, kind},
        new_op,
    },
    transport::Remote,
    zip_bundle,
};

pub use crate::zip_bundle::{Checkout, init_bundle};
pub use checkout::{CheckoutRequest, CheckoutResult};
pub use commit::{CommitOutcome, CommitRequest, ConflictChoice};

pub struct Workspace {
    layout: Layout,
    db: Db,
    _lock: File,
}

impl Workspace {
    /// # Errors
    ///
    /// Returns an error if the workspace cannot be opened or is locked by another operation.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let (layout, lock) = Layout::open(root.as_ref())?;
        let db = Db::open(&layout.index())?;
        Ok(Self {
            layout,
            db,
            _lock: lock,
        })
    }

    /// # Errors
    ///
    /// Returns an error if a tracked entry or the workspace index cannot be inspected.
    pub fn status(&self) -> Result<Vec<Status>> {
        let tracks = self.db.all_tracks()?;
        let pending = self.db.all_pending()?;
        let layout = &self.layout;
        let snapshot = tracks
            .par_iter()
            .map(|entry| status_one(layout, entry))
            .collect::<Result<Vec<_>>>()?;
        let mut statuses: BTreeMap<_, _> = snapshot
            .into_iter()
            .map(|status| (status.path.clone(), status))
            .collect();
        for pending in pending {
            let path = pending.path;
            let bundle = matches!(pending.snapshot, Snapshot::Bundle(_));
            let status = statuses.entry(path.clone()).or_insert(Status {
                path,
                container_id: None,
                version: None,
                bundle,
                state: FileState::Pending,
                op_id: None,
            });
            status.bundle = bundle;
            status.state = FileState::Pending;
            status.op_id = Some(pending.op_id);
        }
        Ok(statuses.into_values().collect())
    }

    /// # Errors
    ///
    /// Returns an error if the server or the workspace index cannot resolve a pending submission.
    pub fn refresh(&self, remote: &Remote) -> Result<()> {
        for pending in self.db.all_pending()? {
            if pending.endpoint == remote.endpoint() {
                self.reconcile(remote, pending)?;
            }
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the path escapes the workspace or uses a symbolic link.
    pub fn local_path(&self, path: &Path) -> Result<PathBuf> {
        self.layout.path(&self.layout.local(path)?)
    }

    fn reconcile(&self, remote: &Remote, pending: PendingCommit) -> Result<Option<Commit>> {
        check_endpoint(remote, &pending.endpoint)?;
        let receipt = match remote.receipt(&pending.op_id) {
            Ok(receipt) => receipt,
            Err(Error::Remote { status: 404, .. }) => {
                self.db.abandon(&pending.path)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let commit: Commit = serde_json::from_value(receipt.result)?;
        self.record_commit(pending, &commit)?;
        Ok(Some(commit))
    }

    fn record_commit(&self, pending: PendingCommit, commit: &Commit) -> Result<()> {
        if pending.op_id != commit.version.op_id {
            return Err(invalid(
                "operation response did not match the pending commit",
            ));
        }
        self.db.save(&Track {
            path: pending.path,
            container_id: commit.container.container_id.clone(),
            version: commit.version.version,
            endpoint: pending.endpoint,
            snapshot: pending.snapshot,
        })
    }

    fn unoccupied(&self, candidate: &LocalPath) -> Result<()> {
        for track in self.db.all_tracks()? {
            let existing = &track.path;
            if candidate == existing {
                continue;
            }
            let candidate = LocalPath::parse(candidate.as_str().to_lowercase())?;
            let lower = LocalPath::parse(existing.as_str().to_lowercase())?;
            if candidate == lower || candidate.below(&lower) || lower.below(&candidate) {
                return Err(Error::LocalConflict(format!(
                    "local path overlaps tracked entry {}",
                    existing.as_str()
                )));
            }
        }
        Ok(())
    }
}

fn check_endpoint(remote: &Remote, endpoint: &str) -> Result<()> {
    if endpoint == remote.endpoint() {
        Ok(())
    } else {
        Err(Error::LocalConflict(format!(
            "local path is tracked by {endpoint}"
        )))
    }
}

fn status_one(layout: &Layout, entry: &Track) -> Result<Status> {
    let target = layout.path(&entry.path)?;
    let bundle = matches!(entry.snapshot, Snapshot::Bundle(_));
    let state = match kind(&target)? {
        None => FileState::Missing,
        Some(EntryKind::File)
            if entry.snapshot == Snapshot::File(crate::store::layout::stamp(&target)?) =>
        {
            FileState::Clean
        }
        Some(EntryKind::Directory) if bundle => {
            if target.join(".zxsignore").is_file()
                && entry.snapshot == Snapshot::Bundle(zip_bundle::collect(&target)?)
            {
                FileState::Clean
            } else {
                FileState::Modified
            }
        }
        _ => FileState::Modified,
    };
    Ok(Status {
        path: entry.path.clone(),
        container_id: Some(entry.container_id.clone()),
        version: Some(entry.version),
        bundle,
        state,
        op_id: None,
    })
}
