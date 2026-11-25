use std::{
    fs::{self, File, FileTimes},
    path::Path,
};

use crate::{
    domain::{BUFFER, Error, Result, invalid},
    store::digest,
};

pub(super) fn copy(from: &Path, to: &Path) -> Result<()> {
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    let mut directories = Vec::new();
    while let Some((source, target)) = pending.pop() {
        let metadata = fs::symlink_metadata(&source)?;
        let kind = metadata.file_type();
        if kind.is_dir() {
            fs::create_dir(&target)?;
            for entry in fs::read_dir(&source)? {
                let entry = entry?;
                pending.push((entry.path(), target.join(entry.file_name())));
            }
            directories.push((target, metadata.permissions()));
        } else if kind.is_file() {
            let output = File::create_new(&target)?;
            let (bytes, _) = digest::copy(File::open(&source)?, &output, BUFFER)?;
            if bytes != metadata.len() {
                return Err(changed(&source));
            }
            output.set_times(FileTimes::new().set_modified(metadata.modified()?))?;
            output.sync_all()?;
            drop(output);
            fs::set_permissions(&target, metadata.permissions())?;
        } else if kind.is_symlink() {
            copy_link(&source, &target, kind)?;
        } else {
            return Err(invalid(format!(
                "无法跨盘移动特殊文件 {}",
                source.display()
            )));
        }
    }
    for (path, permissions) in directories.into_iter().rev() {
        fs::set_permissions(&path, permissions)?;
        #[cfg(unix)]
        File::open(&path)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn verify(from: &Path, to: &Path) -> Result<()> {
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((source, target)) = pending.pop() {
        let a = fs::symlink_metadata(&source)?;
        let b = fs::symlink_metadata(&target)?;
        if a.is_dir() && b.is_dir() {
            let entries = fs::read_dir(&source)?.collect::<std::io::Result<Vec<_>>>()?;
            let target_count =
                fs::read_dir(&target)?.try_fold(0, |count, entry| entry.map(|_| count + 1))?;
            if entries.len() != target_count {
                return Err(changed(&source));
            }
            pending.extend(entries.into_iter().map(|entry| {
                let destination = target.join(entry.file_name());
                (entry.path(), destination)
            }));
        } else if a.is_file() && b.is_file() {
            if a.len() != b.len() || digest::file(&source)? != digest::file(&target)? {
                return Err(changed(&source));
            }
        } else if a.file_type().is_symlink() && b.file_type().is_symlink() {
            if fs::read_link(&source)? != fs::read_link(&target)? {
                return Err(changed(&source));
            }
        } else {
            return Err(changed(&source));
        }
    }
    Ok(())
}

fn changed(path: &Path) -> Error {
    Error::LocalConflict(format!(
        "工作区内容在迁移期间改变，原内容已保留 · {}",
        path.display()
    ))
}

#[cfg(unix)]
fn copy_link(source: &Path, target: &Path, _kind: fs::FileType) -> Result<()> {
    Ok(std::os::unix::fs::symlink(fs::read_link(source)?, target)?)
}

#[cfg(windows)]
fn copy_link(source: &Path, target: &Path, kind: fs::FileType) -> Result<()> {
    use std::os::windows::fs::{FileTypeExt, symlink_dir, symlink_file};

    let link = fs::read_link(source)?;
    if kind.is_symlink_dir() {
        symlink_dir(link, target)?;
    } else {
        symlink_file(link, target)?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn copy_link(source: &Path, _target: &Path, _kind: fs::FileType) -> Result<()> {
    Err(invalid(format!(
        "当前文件系统无法复制链接 {}",
        source.display()
    )))
}
