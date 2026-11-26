use serde::Serialize;
use std::path::Path;

use crate::{
    domain::{
        Container, ContainerId, ContainerPage, Detail, Error, FileName, Filter, Folder, FolderPath,
        Patch, RecycleFilter, RecycleId, RecyclePage, Recycled, Restore, Result,
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
    pub conflicts: Vec<ContainerId>,
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
    /// Returns an error if the server list or ZIP directory cannot be read.
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
        let page = self.remote.list(&filter)?;
        let bundles = self.classify(&page.containers, true)?;
        let direct = filter
            .folder
            .as_ref()
            .filter(|_| filter.cursor.is_none())
            .map(|parent| (parent, folders.as_slice()));
        let conflicts = self.projection_conflicts(&page.containers, &bundles, direct)?;
        Ok(BrowsePage {
            listing: Classified {
                page,
                bundles,
                conflicts,
            },
            folders,
        })
    }

    /// # Errors
    /// Returns an error if the container or ZIP directory cannot be read.
    pub fn detail(&self, id: &ContainerId) -> Result<Classified<Detail>> {
        let page = self.remote.detail(id)?;
        let bundles = self.classify([&page.container], true)?;
        let conflicts =
            self.projection_conflicts(std::slice::from_ref(&page.container), &bundles, None)?;
        Ok(Classified {
            page,
            bundles,
            conflicts,
        })
    }

    /// # Errors
    /// Returns an error if the header needed to identify a ZIP workspace cannot be read.
    pub fn is_bundle(&self, container: &Container) -> Result<bool> {
        Ok(!self.classify([container], true)?.is_empty())
    }

    /// # Errors
    /// Returns an error if the recycle bin or workspace index cannot be read.
    pub fn recycle(&self, filter: &RecycleFilter) -> Result<Classified<RecyclePage>> {
        let page = self.remote.recycle(filter)?;
        let bundles = self.classify(page.entries.iter().map(|entry| &entry.container), false)?;
        Ok(Classified {
            page,
            bundles,
            conflicts: Vec::new(),
        })
    }

    /// # Errors
    /// Returns an error if the edited path conflicts with a directory or workspace.
    pub fn patch(&self, id: &ContainerId, patch: &Patch) -> Result<Container> {
        if patch.folder.is_some() || patch.name.is_some() {
            let original = self.remote.detail(id)?.container;
            let folder = patch.folder.as_ref().unwrap_or(&original.folder);
            let name = patch.name.as_ref().unwrap_or(&original.name);
            let bundle = zip_stem(name.as_str()).is_some() && self.probe_latest(&original)?;
            self.validate_destination(folder, name, bundle)?;
        }
        self.remote.patch(id, patch)
    }

    /// # Errors
    /// Returns an error if the recycle entry is missing or its restored path conflicts.
    pub fn restore(&self, id: &RecycleId, restore: &Restore) -> Result<Container> {
        let entry = self.recycle_entry(id)?;
        let folder = restore.folder.as_ref().unwrap_or(&entry.container.folder);
        let name = restore.name.as_ref().unwrap_or(&entry.container.name);
        let bundle =
            zip_stem(name.as_str()).is_some() && self.known_bundle(&entry.container)? == Some(true);
        self.validate_destination(folder, name, bundle)?;
        self.remote.restore(id, restore)
    }

    pub(super) fn validate_destination(
        &self,
        folder: &FolderPath,
        name: &FileName,
        bundle: bool,
    ) -> Result<()> {
        self.check_folder_destination(folder)?;
        if bundle && zip_stem(name.as_str()).is_some() {
            self.check_bundle_name(folder, name)?;
        }
        Ok(())
    }

    fn recycle_entry(&self, id: &RecycleId) -> Result<Recycled> {
        let mut filter = RecycleFilter {
            limit: Some(200),
            cursor: None,
        };
        loop {
            let page = self.remote.recycle(&filter)?;
            if let Some(entry) = page
                .entries
                .into_iter()
                .find(|entry| &entry.recycle.recycle_id == id)
            {
                return Ok(entry);
            }
            let Some(cursor) = page.next_cursor else {
                return Err(Error::Remote {
                    status: 404,
                    message: "recycle entry not found".into(),
                });
            };
            filter.cursor = Some(cursor);
        }
    }
}

mod folders;
mod projection;
