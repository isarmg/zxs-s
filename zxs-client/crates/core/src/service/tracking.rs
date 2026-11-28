use std::path::Path;

use crate::domain::{ContainerId, Recycled};

use super::{Error, LocalPath, Remote, Result, VersionNum, Workspace, invalid, kind};

impl Workspace {
    /// Releases local tracking and keeps the local files.
    /// # Errors
    /// Returns an error if the path is not tracked or the index cannot be updated.
    pub fn untrack(&self, path: &Path) -> Result<()> {
        if self.db.forget(&self.layout.local(path)?)? {
            Ok(())
        } else {
            Err(invalid("the local path is not tracked"))
        }
    }

    /// Moves a missing tracked entry to the server recycle bin.
    /// # Errors
    /// Returns an error if the entry is present locally or the expected container or version changed.
    pub fn sync_delete(
        &self,
        remote: &Remote,
        path: &Path,
        (expected_id, expected_version): (Option<&ContainerId>, Option<VersionNum>),
    ) -> Result<Recycled> {
        let path = self.layout.local(path)?;
        let track = self
            .db
            .track(&path)?
            .ok_or_else(|| invalid("the local file is not tracked"))?;
        if expected_id.is_some_and(|id| id != &track.container_id) {
            return Err(invalid("本地追踪对象已更换，请刷新工作区后重试"));
        }
        if self.db.interrupted_checkout(&track.container_id)?.is_some() {
            return Err(Error::CheckoutInterrupted);
        }
        if kind(&self.layout.checkout_path(&path, true)?)?.is_some() {
            return Err(Error::LocalConflict(
                "同步删除仅适用于本地已缺失的追踪项".into(),
            ));
        }
        if track.endpoint != remote.endpoint() {
            self.tracked_container(remote, &track)?;
        }
        let version = expected_version.unwrap_or(track.version);
        match remote.recycle_operation(&track.container_id, Some(version)) {
            Ok(entry) => self.db.forget(&path).map(|_| entry),
            Err(Error::Remote { status: 404, .. }) => Err(self.mark_missing(&path)?),
            Err(error) => Err(error),
        }
    }

    pub(super) fn mark_missing(&self, path: &LocalPath) -> Result<Error> {
        self.db.remote_missing(path, true)?;
        Ok(Error::Remote {
            status: 404,
            message: format!("{} 的远端文件当前不可用", path.as_str()),
        })
    }
}
