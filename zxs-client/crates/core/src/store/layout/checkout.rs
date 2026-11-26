use std::path::{Path, PathBuf};

use crate::domain::{EntryKind, Error, Result, invalid};

use super::{Layout, WORKSPACE_DIR, create_private, kind, random_hex};

impl Layout {
    pub fn checkout_storage(&self, directory: &Path) -> Result<PathBuf> {
        for ancestor in directory.ancestors() {
            match kind(ancestor)? {
                Some(EntryKind::Directory) => return self.checkout_volume(ancestor),
                Some(EntryKind::File) => {
                    return Err(Error::LocalConflict(format!(
                        "本地目标的上级路径是文件 {}",
                        ancestor.display()
                    )));
                }
                None => {}
            }
        }
        Err(invalid("cannot find checkout volume"))
    }

    fn checkout_volume(&self, directory: &Path) -> Result<PathBuf> {
        if same_volume(directory, &self.system)? {
            return Ok(self.system.clone());
        }
        let storage = directory.join(WORKSPACE_DIR);
        create_private(&storage)?;
        Ok(storage)
    }

    pub fn checkout_backup(&self, directory: &Path) -> Result<PathBuf> {
        let backups = self.checkout_storage(directory)?.join("backups");
        create_private(&backups)?;
        Ok(backups.join(random_hex::<16>()?))
    }
}

#[cfg(unix)]
fn same_volume(left: &Path, right: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(left.metadata()?.dev() == right.metadata()?.dev())
}

#[cfg(windows)]
pub(super) fn same_volume(left: &Path, right: &Path) -> Result<bool> {
    fn volume(path: &Path) -> Result<String> {
        let path = path
            .to_str()
            .ok_or_else(|| invalid("checkout directory is not UTF-8"))?;
        winsafe::GetVolumePathName(path)
            .map_err(|error| invalid(format!("cannot read checkout volume: {error}")))
    }
    Ok(volume(left)?.eq_ignore_ascii_case(&volume(right)?))
}
