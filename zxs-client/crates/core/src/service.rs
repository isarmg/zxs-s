mod browse;
mod bundle_workspace;
mod catalog;
mod checkout;
mod commit;
mod inspect;
mod relocate;
mod sync;
#[cfg(test)]
mod tests;
mod tracking;

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use crate::{
    domain::{
        Commit, Container, ContainerId, EntryKind, Error, FileName, FileState, FolderPath,
        LocalPath, NewContainer, Result, Snapshot, Status, Track, VersionNum, invalid,
    },
    store::{
        db::Db,
        layout::{Layout, kind},
    },
    transport::Remote,
    zip_bundle,
};

pub use crate::zip_bundle::Checkout;
pub use catalog::Catalog;
pub use checkout::{CheckoutRequest, CheckoutResult};
pub use commit::{CommitOutcome, CommitRequest, ConflictChoice};
use inspect::status_one;
pub use sync::SyncReport;

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
    /// Returns an error if the path escapes the workspace or uses a symbolic link.
    pub fn local_path(&self, path: &Path) -> Result<PathBuf> {
        self.layout.path(&self.layout.local(path)?)
    }

    fn unoccupied(&self, candidate: &LocalPath) -> Result<()> {
        self.check_local_target(candidate, None)
    }

    fn check_local_target(&self, candidate: &LocalPath, except: Option<&LocalPath>) -> Result<()> {
        if let Some(existing) = self.db.overlapping_path(candidate, except)? {
            return Err(Error::LocalConflict(format!(
                "local path overlaps tracked entry {}",
                existing.as_str()
            )));
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
