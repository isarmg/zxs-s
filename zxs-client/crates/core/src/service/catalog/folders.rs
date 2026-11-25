use crate::{
    domain::{Container, Error, FileName, Filter, Folder, FolderMove, FolderPath, Result, invalid},
    zip_bundle::zip_stem,
};

use super::Catalog;

impl Catalog<'_> {
    /// # Errors
    /// Returns an error if the requested folder conflicts with an existing folder, file, or ZIP workspace.
    pub fn create_folder(&self, folder: FolderPath) -> Result<Folder> {
        folder.require_child()?;
        self.check_folder_destination(&folder)?;
        self.remote.create_folder(&Folder { folder })
    }

    /// # Errors
    /// Returns an error if the source cannot move safely to the requested destination.
    pub fn move_folder(&self, request: &FolderMove) -> Result<Folder> {
        request.from.require_child()?;
        request.to.require_child()?;
        self.check_folder_destination(&request.to)?;
        self.remote.move_folder(request)
    }

    /// # Errors
    /// Returns an error if the folder is missing or contains a file or folder.
    pub fn delete_folder(&self, folder: &FolderPath, recursive: bool) -> Result<Folder> {
        folder.require_child()?;
        self.remote.delete_folder(folder, recursive)
    }

    pub(crate) fn children(&self, path: &FolderPath) -> Result<Vec<Folder>> {
        match self.remote.folders(path) {
            Ok(page) => Ok(page.folders),
            Err(Error::Remote { status: 404, .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn check_bundle_name(&self, folder: &FolderPath, name: &FileName) -> Result<()> {
        if let Some(existing) = self.same_folder(folder, name)? {
            return Err(Error::LocalConflict(format!(
                "已存在同名文件夹 {existing}，请先改名或指定不同的路径"
            )));
        }
        Ok(())
    }

    pub(super) fn same_folder(
        &self,
        folder: &FolderPath,
        name: &FileName,
    ) -> Result<Option<FolderPath>> {
        Ok(matching_projection(&self.children(folder)?, folder, name).cloned())
    }

    pub(crate) fn check_folder_destination(&self, folder: &FolderPath) -> Result<()> {
        self.check_disk(folder)?;
        let mut target = folder.clone();
        while let Some(parent) = target
            .parent()
            .filter(|parent| parent != &FolderPath::root())
        {
            self.check_zip_sibling(&parent, &target)?;
            target = parent;
        }
        Ok(())
    }

    pub(crate) fn check_disk(&self, folder: &FolderPath) -> Result<()> {
        let root = FolderPath::root();
        let disk = folder.disk()?;
        let expected = FolderPath::parse(format!("/{disk}"))?;
        if self
            .remote
            .folders(&root)?
            .folders
            .iter()
            .any(|folder| folder.folder == expected)
        {
            return Ok(());
        }
        Err(invalid(format!("服务器没有登记数据盘 {disk}")))
    }

    pub(crate) fn find_name(
        &self,
        folder: &FolderPath,
        name: &FileName,
    ) -> Result<Option<Container>> {
        let key = name.key();
        let mut filter = Filter {
            folder: Some(folder.clone()),
            q: Some(key.clone()),
            ..Filter::default()
        };
        loop {
            let page = self.remote.list(&filter)?;
            if let Some(container) = page
                .containers
                .into_iter()
                .find(|entry| entry.name.key() == key)
            {
                return Ok(Some(container));
            }
            let Some(cursor) = page.next_cursor else {
                return Ok(None);
            };
            filter.cursor = Some(cursor);
        }
    }

    fn check_zip_sibling(&self, parent: &FolderPath, target: &FolderPath) -> Result<()> {
        let name = target.as_str().rsplit('/').next().unwrap_or("");
        let Ok(zip) = FileName::parse(format!("{name}.zip")) else {
            return Ok(());
        };
        if let Some(container) = self.find_name(parent, &zip)?
            && !self.classify([&container], true)?.is_empty()
        {
            return Err(Error::LocalConflict(format!(
                "已存在同名目录工作区 {}，无法创建文件夹 {target}",
                container.name
            )));
        }
        Ok(())
    }
}

pub(super) fn matching_projection<'f>(
    children: &'f [Folder],
    parent: &FolderPath,
    name: &FileName,
) -> Option<&'f FolderPath> {
    let stem = zip_stem(name.as_str()).filter(|stem| !stem.is_empty())?;
    let target = format!("{}/{stem}", parent.as_str().trim_end_matches('/')).to_lowercase();
    children
        .iter()
        .find(|child| child.folder.as_str().to_lowercase() == target)
        .map(|item| &item.folder)
}
