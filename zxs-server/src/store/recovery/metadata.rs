use std::path::Path;

use crate::domain::{Change, Container, ContainerMeta, Error, Pending, Result, TxId};
use crate::store::{StageGuard, fs_ops::atomic_json};

use super::{Layout, Store, mkdir, parent, read_json, rename};

enum Staging {
    Upload,
    CopiedContainer,
    ExistingContainer,
}

impl Store {
    pub fn prepare_metadata(&self, pending: &Pending) -> Result<Option<StageGuard>> {
        let (container, staging) = match &pending.change {
            Change::Publish { commit } => (&commit.container, Staging::Upload),
            Change::Patch {
                container, staged, ..
            }
            | Change::Restore {
                container, staged, ..
            } => {
                let staging = if *staged {
                    Staging::CopiedContainer
                } else {
                    Staging::ExistingContainer
                };
                (container, staging)
            }
            _ => return Ok(None),
        };
        let tx = &pending.tx_id;
        let disk = self.paths.data_disk(&container.folder)?;
        let guard = match staging {
            Staging::ExistingContainer => Some(self.stage(&disk, tx)?),
            Staging::Upload | Staging::CopiedContainer => None,
        };
        let path = match staging {
            Staging::CopiedContainer => Layout::container_meta(&self.paths.staged_tree(&disk, tx)),
            Staging::Upload | Staging::ExistingContainer => self.paths.prepared_metadata(&disk, tx),
        };
        atomic_json(&path, &container.metadata())?;
        Ok(guard)
    }

    pub(super) fn install_metadata(
        &self,
        destination: &Path,
        container: &Container,
        tx: &TxId,
    ) -> Result<()> {
        let metadata = Layout::container_meta(destination);
        let staged = self.paths.prepared_metadata(&container.folder.disk()?, tx);
        if staged.try_exists()? {
            mkdir(parent(&metadata)?)?;
            rename(&staged, &metadata)?;
        }
        let installed: ContainerMeta = read_json(&metadata)?;
        if installed != container.metadata() {
            return Err(Error::Recovery);
        }
        Ok(())
    }
}
