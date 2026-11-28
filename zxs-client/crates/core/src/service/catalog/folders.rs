use crate::domain::{Container, Error, FileName, Filter, Folder, FolderPath, Result, invalid};

use crate::transport::Remote;

impl Remote {
    pub(crate) fn children(&self, path: &FolderPath) -> Result<Vec<Folder>> {
        match self.folders(path) {
            Ok(page) => Ok(page.folders),
            Err(Error::Remote { status: 404, .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn check_disk(&self, folder: &FolderPath) -> Result<()> {
        let disk = folder.disk()?;
        let expected = format!("/{disk}");
        if self
            .folders(&FolderPath::root())?
            .folders
            .iter()
            .any(|folder| folder.folder.as_str() == expected)
        {
            return Ok(());
        }
        Err(invalid(format!("服务器没有登记数据盘 {disk}")))
    }

    /// Looks up one exact file name in a directory.
    ///
    /// # Errors
    /// Returns an error if the server listing cannot be read.
    pub fn find_name(&self, folder: &FolderPath, name: &FileName) -> Result<Option<Container>> {
        let key = name.key();
        let mut filter = Filter {
            folder: Some(folder.clone()),
            q: Some(key.clone()),
            ..Filter::default()
        };
        loop {
            let page = self.list(&filter)?;
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
