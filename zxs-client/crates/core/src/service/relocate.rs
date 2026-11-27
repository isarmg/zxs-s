use std::{fs, path::Path};

use super::{
    Container, Error, LocalPath, Result, Snapshot, Track, Workspace,
    inspect::{same_contents, status_one},
};
use crate::domain::FileState;
use crate::store::layout::{Layout, kind, move_new, parent};

impl Workspace {
    pub(super) fn moved_mirror(
        &self,
        track: &Track,
        container: &Container,
    ) -> Result<Option<LocalPath>> {
        if !track.mirrored {
            return Ok(None);
        }
        let target = self.destination(container)?;
        let follows = track.path == target
            || (matches!(track.snapshot, Snapshot::Bundle(_))
                && crate::zip_bundle::matches_bundle_stem(&track.path, &target));
        Ok((!follows).then_some(target))
    }

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
        let destination = self.layout.checkout_path(target, false)?;
        let case_only = track.path.key() == target.key();
        if !case_only && kind(&destination)?.is_some() {
            return Err(Error::LocalConflict(format!(
                "本地目标路径已存在 {}",
                destination.display()
            )));
        }
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

    fn require_movable(&self, track: &Track) -> Result<()> {
        if !matches!(status_one(&self.layout, track)?.state, FileState::Clean)
            || !same_contents(&self.layout, track)?
        {
            return Err(Error::LocalConflict(
                "内容已改变，请先提交或备份再移动".into(),
            ));
        }
        Ok(())
    }
}

fn move_path(layout: &Layout, from: &Path, to: &Path, case_only: bool) -> Result<()> {
    if case_only {
        fs::create_dir_all(parent(to)?)?;
        layout.rename_case(from, to)
    } else {
        move_new(from, to)
    }
}
