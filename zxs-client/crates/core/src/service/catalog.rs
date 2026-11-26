use serde::Serialize;
use std::path::Path;

use crate::{
    domain::{
        Container, ContainerId, ContainerPage, Detail, Filter, Folder, FolderPath, RecycleFilter,
        RecyclePage, Result, Snapshot,
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
        let mut filter = filter.clone();
        if filter.tag.is_some() || filter.q.is_some() {
            filter.folder = None;
        }
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
            self.remote.list(&filter)?
        };
        let bundles = self.classify(&page.containers)?;
        Ok(BrowsePage {
            listing: Classified { page, bundles },
            folders,
        })
    }

    /// # Errors
    /// Returns an error if the container metadata cannot be read.
    pub fn detail(&self, id: &ContainerId) -> Result<Classified<Detail>> {
        let page = self.remote.detail(id)?;
        let bundles = self.classify([&page.container])?;
        Ok(Classified { page, bundles })
    }

    /// # Errors
    /// Returns an error if the recycle bin or workspace index cannot be read.
    pub fn recycle(&self, filter: &RecycleFilter) -> Result<Classified<RecyclePage>> {
        let page = self.remote.recycle(filter)?;
        let bundles = self.classify(page.entries.iter().map(|entry| &entry.container))?;
        Ok(Classified { page, bundles })
    }

    fn classify<'c>(
        &self,
        containers: impl IntoIterator<Item = &'c Container>,
    ) -> Result<Vec<ContainerId>> {
        let Some(index) = &self.index else {
            return Ok(Vec::new());
        };
        let mut bundles = Vec::new();
        for container in containers {
            if zip_stem(container.name.as_str()).is_some()
                && index
                    .track_for_container(&container.container_id)?
                    .is_some_and(|track| {
                        track.version == container.latest_version
                            && track.restored_from.is_none()
                            && track.baseline_hash() == Some(&container.latest_blake3)
                            && matches!(track.snapshot, Snapshot::Bundle(_))
                    })
            {
                bundles.push(container.container_id.clone());
            }
        }
        Ok(bundles)
    }
}

mod folders;
