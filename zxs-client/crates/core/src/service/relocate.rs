use std::{fs, path::Path};

use super::{Container, Error, LocalPath, Result, Track, Workspace, inspect::clean_contents};
use crate::store::layout::{Layout, kind, move_new, parent, same_existing_path};

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
        Ok((track.path != target).then_some(target))
    }

    pub(super) fn relocate(&self, track: &mut Track, target: &LocalPath) -> Result<()> {
        self.require_outside_bundle(target.parent().as_ref())?;
        if !clean_contents(&self.layout, track)? {
            return Err(Error::LocalConflict(
                "内容已改变，请先提交或备份再移动".into(),
            ));
        }
        self.check_local_target(target, Some(&track.container_id))?;
        let source = self.layout.path(&track.path)?;
        let case_only = track.path.key() == target.key();
        let destination = self.layout.checkout_path(target, !case_only)?;
        if !case_only && kind(&destination)?.is_some() {
            return Err(Error::LocalConflict(format!(
                "本地目标路径已存在 {}",
                destination.display()
            )));
        }
        let aliased = case_only
            && source.file_name() == destination.file_name()
            && same_existing_path(&destination, &source);
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
}

fn move_path(layout: &Layout, from: &Path, to: &Path, case_only: bool) -> Result<()> {
    if case_only {
        fs::create_dir_all(parent(to)?)?;
        layout.rename_case(from, to)
    } else {
        move_new(from, to)
    }
}
