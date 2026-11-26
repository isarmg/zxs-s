use crate::domain::{
    Container, Error, FileName, Filter, Folder, FolderMove, FolderPath, Result, invalid,
};

use super::Catalog;

impl Catalog<'_> {
    /// # Errors
    /// Returns an error if the requested folder conflicts with an existing folder, file, or ZIP workspace.
    pub fn create_folder(&self, folder: FolderPath) -> Result<Folder> {
        self.remote
            .create_folder(&Folder { folder })
            .map(|(folder, _)| folder)
    }

    /// # Errors
    /// Returns an error if the source cannot move safely to the requested destination.
    pub fn move_folder(&self, request: &FolderMove) -> Result<Folder> {
        request.from.require_child()?;
        request.to.require_child()?;
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
}
