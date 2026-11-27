use std::path::Path;

use crate::domain::{Change, Container, Error, Pending, Result, TxId, VersionNum};
use crate::store::{
    StageGuard,
    fs_ops::{atomic_json, container_meta},
};

use super::{Layout, Store, rename};

enum Staging {
    Upload,
    CopiedContainer,
    ExistingContainer,
}

impl Store {
    pub fn prepare_metadata(&self, pending: &Pending) -> Result<Option<StageGuard>> {
        let (container, staging, first) = match &pending.change {
            Change::Publish { commit } => {
                let first = if commit.version.number == VersionNum::FIRST {
                    VersionNum::FIRST
                } else {
                    stored_first(&self.paths.container(&commit.container))?
                };
                (&commit.container, Staging::Upload, first)
            }
            Change::Patch {
                previous,
                container,
                staged,
            } => {
                let first = stored_first(&self.paths.container(previous))?;
                (container, Staging::for_container(*staged), first)
            }
            Change::Restore {
                container,
                entry,
                staged,
            } => {
                let batch = self.paths.recycled_entry(entry);
                let source = Layout::recycled_container(&batch, &entry.container.name);
                (
                    container,
                    Staging::for_container(*staged),
                    stored_first(&source)?,
                )
            }
            Change::Prune {
                container,
                first_retained_version,
            } => (
                container,
                Staging::ExistingContainer,
                *first_retained_version,
            ),
            _ => return Ok(None),
        };
        let tx = &pending.tx_id;
        let disk = self.paths.data_disk(&container.folder)?;
        let guard = match staging {
            Staging::ExistingContainer => Some(self.stage(&disk, tx)?),
            Staging::Upload | Staging::CopiedContainer => None,
        };
        let path = match staging {
            Staging::CopiedContainer => Layout::metadata(&self.paths.staged_tree(&disk, tx)),
            Staging::Upload | Staging::ExistingContainer => self.paths.prepared_metadata(&disk, tx),
        };
        atomic_json(&path, &container.metadata(first))?;
        Ok(guard)
    }

    pub(super) fn install_metadata(
        &self,
        destination: &Path,
        container: &Container,
        tx: &TxId,
    ) -> Result<()> {
        let metadata = Layout::metadata(destination);
        let staged = self.paths.prepared_metadata(&container.folder.disk()?, tx);
        if staged.try_exists()? {
            rename(&staged, &metadata)?;
        }
        let installed = container_meta(destination)?;
        if installed != container.metadata(installed.first_retained_version) {
            return Err(Error::Recovery);
        }
        Ok(())
    }
}

impl Staging {
    fn for_container(staged: bool) -> Self {
        if staged {
            Self::CopiedContainer
        } else {
            Self::ExistingContainer
        }
    }
}

fn stored_first(path: &Path) -> Result<VersionNum> {
    Ok(container_meta(path)?.first_retained_version)
}
