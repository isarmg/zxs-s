use std::path::Path;

use crate::domain::Recycled;

use super::{Error, LocalPath, Remote, Result, VersionNum, Workspace, invalid, kind};

impl Workspace {
    /// Releases local tracking and keeps the local files.
    /// # Errors
    /// Returns an error if the path is not tracked or the index cannot be updated.
    pub fn untrack(&self, path: &Path) -> Result<()> {
        let path = self.layout.local(path)?;
        if self.db.track(&path)?.is_none()
            && !self
                .db
                .all_checkouts()?
                .iter()
                .any(|entry| entry.path == path)
        {
            return Err(invalid("the local path is not tracked"));
        }
        self.db.forget(&path)
    }

    /// Moves a missing tracked entry to the server recycle bin.
    /// # Errors
    /// Returns an error if the entry is present locally or the expected server version has changed.
    pub fn sync_delete(
        &self,
        remote: &Remote,
        path: &Path,
        expected: Option<VersionNum>,
    ) -> Result<Recycled> {
        let path = self.layout.local(path)?;
        let track = self
            .db
            .track(&path)?
            .ok_or_else(|| invalid("the local file is not tracked"))?;
        if self.db.interrupted_checkout(&track.container_id)?.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        if kind(&self.layout.path(&path)?)?.is_some() {
            return Err(Error::LocalConflict(
                "同步删除仅适用于本地已缺失的追踪项".into(),
            ));
        }
        if track.endpoint != remote.endpoint() {
            self.tracked_container(remote, &track)?;
        }
        match remote.recycle_operation(&track.container_id, Some(expected.unwrap_or(track.version)))
        {
            Ok(entry) => {
                self.db.forget(&path)?;
                Ok(entry)
            }
            Err(Error::Remote { status: 404, .. }) => Err(self.mark_missing(&path)?),
            Err(error) => Err(error),
        }
    }

    pub(super) fn mark_missing(&self, path: &LocalPath) -> Result<Error> {
        self.db.remote_missing(path, true)?;
        Ok(Error::Remote {
            status: 404,
            message: format!(
                "远端文件已不存在。可到回收站还原，或用 untrack 解除 {} 的追踪并保留本地文件",
                path.as_str()
            ),
        })
    }
}
