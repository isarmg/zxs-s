#[cfg(any(windows, target_os = "macos"))]
use std::fs;
#[cfg(any(windows, target_os = "macos", test))]
use std::path::Path;

use super::Workspace;
#[cfg(any(windows, target_os = "macos"))]
use crate::domain::EntryKind;
use crate::domain::{Error, FileState, Result, Track};
#[cfg(any(windows, target_os = "macos", test))]
use crate::store::layout::kind;

impl Workspace {
    pub(in crate::service) fn require_movable(&self, track: &Track) -> Result<()> {
        use crate::service::inspect::{same_contents, status_one};

        if !matches!(status_one(&self.layout, track)?.state, FileState::Clean)
            || !same_contents(&self.layout, track)?
        {
            return Err(Error::LocalConflict(
                "内容已改变，请先提交或备份再移动".into(),
            ));
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub(super) fn materialize_spelling(&self, path: &Path) -> Result<()> {
        let root = &self.layout.root;
        let parts = path.strip_prefix(root).map_err(|_| unverified(path))?;
        let mut target = root.clone();
        for part in parts {
            target.push(part);
            match kind(&target)? {
                None => fs::create_dir(&target)?,
                Some(EntryKind::File) => return Err(unverified(&target)),
                Some(EntryKind::Directory) => self.adopt_spelling(&target)?,
            }
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn adopt_spelling(&self, target: &Path) -> Result<()> {
        use unicode_normalization::UnicodeNormalization;

        let actual = target.canonicalize()?;
        let shown = actual.file_name().and_then(|name| name.to_str());
        let requested = target.file_name().and_then(|name| name.to_str());
        if let (Some(shown), Some(requested)) = (shown, requested)
            && !shown.nfc().eq(requested.chars())
            && shown.nfc().collect::<String>().to_lowercase() == requested.to_lowercase()
        {
            let original = target.with_file_name(shown);
            self.check_folder_contents(&original)?;
            self.layout.rename_case(&original, target)?;
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "macos", test))]
    pub(in crate::service) fn check_folder_contents(&self, folder: &Path) -> Result<()> {
        let local = self.layout.local(folder)?;
        for track in self.db.tracks_in(&local)? {
            let path = self.layout.path(&track.path)?;
            if self.db.interrupted_checkout(&track.container_id)?.is_some() {
                return Err(unverified(&path));
            }
            if kind(&path)?.is_some() {
                self.require_movable(&track)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn unverified(path: &Path) -> Error {
    Error::LocalConflict(format!(
        "无法确认可安全移动，请处理本地内容 {}",
        path.display()
    ))
}
