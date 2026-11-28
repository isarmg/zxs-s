use serde::Serialize;
use std::path::Path;

use crate::{
    domain::{
        Blake3Hash, Container, ContainerId, ContainerPage, Detail, FileName, Filter, Folder,
        FolderPath, RecycleFilter, RecyclePage, Result, Snapshot, VersionNum,
    },
    store::{db::Db, layout::Layout},
    transport::Remote,
    zip_bundle::zip_stem,
};

#[derive(Serialize)]
pub struct Classified<T> {
    #[serde(flatten)]
    pub page: T,
    pub bundles: Vec<ContainerId>,
}

#[derive(Serialize)]
pub struct BrowsePage {
    #[serde(flatten)]
    pub(crate) listing: Classified<ContainerPage>,
    pub(crate) folders: Vec<Folder>,
}

pub struct Catalog<'a> {
    remote: &'a Remote,
    index: Option<Db>,
}

impl<'a> Catalog<'a> {
    /// # Errors
    /// Returns an error if the workspace index exists but cannot be read.
    pub fn open(remote: &'a Remote, workspace: Option<&Path>) -> Result<Self> {
        let index = if let Some(root) = workspace {
            Db::read_only(&Layout::index_at(root))?
        } else {
            None
        };
        Ok(Self { remote, index })
    }

    /// # Errors
    /// Returns an error if the server directory or local tracking records cannot be read.
    pub fn list(&self, filter: &Filter) -> Result<BrowsePage> {
        let folders = if let Some(parent) = &filter.folder
            && filter.cursor.is_none()
        {
            self.remote.folders(parent)?.folders
        } else {
            Vec::new()
        };
        let page = if filter.folder.as_ref() == Some(&FolderPath::root()) {
            ContainerPage {
                containers: Vec::new(),
                next_cursor: None,
            }
        } else {
            self.remote.list(filter)?
        };
        let bundles = self.classify(page.containers.iter().map(bundle_fields))?;
        Ok(BrowsePage {
            listing: Classified { page, bundles },
            folders,
        })
    }

    /// # Errors
    /// Returns an error if the container metadata cannot be read.
    pub fn detail(&self, id: &ContainerId) -> Result<Classified<Detail>> {
        let page = self.remote.detail(id)?;
        let bundles = self.classify([bundle_fields(&page.container)])?;
        Ok(Classified { page, bundles })
    }

    /// # Errors
    /// Returns an error if the recycle bin or workspace index cannot be read.
    pub fn recycle(&self, filter: &RecycleFilter) -> Result<Classified<RecyclePage>> {
        let page = self.remote.recycle(filter)?;
        let bundles = self.classify(page.entries.iter().map(|entry| {
            let container = &entry.container;
            (
                &container.container_id,
                &container.name,
                container.latest_version,
                &container.latest_blake3,
            )
        }))?;
        Ok(Classified { page, bundles })
    }

    fn classify<'c>(
        &self,
        containers: impl IntoIterator<
            Item = (&'c ContainerId, &'c FileName, VersionNum, &'c Blake3Hash),
        >,
    ) -> Result<Vec<ContainerId>> {
        let Some(index) = &self.index else {
            return Ok(Vec::new());
        };
        let mut bundles = Vec::new();
        for (id, name, version, hash) in containers {
            if zip_stem(name.as_str()).is_some()
                && index.track_for_container(id)?.is_some_and(|track| {
                    track.version == version
                        && track.restored_from.is_none()
                        && track.baseline_hash() == Some(hash)
                        && matches!(track.snapshot, Snapshot::Bundle(_))
                })
            {
                bundles.push(id.clone());
            }
        }
        Ok(bundles)
    }
}

fn bundle_fields(container: &Container) -> (&ContainerId, &FileName, VersionNum, &Blake3Hash) {
    (
        &container.container_id,
        &container.name,
        container.latest_version,
        &container.latest_blake3,
    )
}

mod folders;
