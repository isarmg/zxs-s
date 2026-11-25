mod tree;

use std::{fs, path::Path};

use super::{Error, LocalPath, Result, Track, Workspace, invalid};
use crate::store::layout::{Layout, kind, parent};

impl Workspace {
    pub(super) fn relocate(&self, track: &mut Track, target: &LocalPath) -> Result<()> {
        self.require_movable(track)?;
        self.check_local_target(target, Some(&track.path))?;
        if self
            .db
            .track(target)?
            .is_some_and(|other| other.container_id != track.container_id)
        {
            return Err(Error::LocalConflict(
                "目标路径或原路径有未处理的本地状态".into(),
            ));
        }
        let source = self.layout.path(&track.path)?;
        let destination = self.layout.path(target)?;
        let case_only = track.path.key() == target.key();
        if !case_only && kind(&destination)?.is_some() {
            return Err(Error::LocalConflict(format!(
                "本地目标路径已存在 {}",
                destination.display()
            )));
        }
        self.materialize_folder(parent(&destination)?)?;
        let aliased = case_only
            && source.file_name() == destination.file_name()
            && destination.canonicalize().is_ok_and(|actual| {
                source
                    .canonicalize()
                    .is_ok_and(|previous| previous == actual)
            });
        if !aliased {
            move_path(&self.layout, &source, &destination, case_only)?;
        }
        let old = std::mem::replace(&mut track.path, target.clone());
        if let Err(error) = self.db.relocate(&old, track) {
            if !aliased {
                move_path(&self.layout, &destination, &source, case_only)?;
            }
            track.path = old;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn materialize_folder(&self, path: &Path) -> Result<()> {
        if !path.starts_with(&self.layout.root) {
            return Err(invalid("mirror folder is outside the workspace"));
        }
        #[cfg(any(windows, target_os = "macos"))]
        self.materialize_spelling(path)?;
        #[cfg(not(any(windows, target_os = "macos")))]
        fs::create_dir_all(path)?;
        Ok(())
    }
}

fn move_path(layout: &Layout, from: &Path, to: &Path, case_only: bool) -> Result<()> {
    if case_only {
        layout.rename_case(from, to)
    } else {
        Ok(fs::rename(from, to)?)
    }
}
