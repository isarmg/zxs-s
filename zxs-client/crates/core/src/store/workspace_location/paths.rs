use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
};

use crate::domain::{Error, Result};

#[cfg(windows)]
pub(super) use super::windows::rename_directory;

pub(super) fn same_directory(from: &Path, to: &Path) -> Result<bool> {
    match fs::symlink_metadata(to) {
        Ok(metadata) if metadata.is_dir() && matches_directory(from, to, &metadata)? => Ok(true),
        Ok(_) => Err(Error::LocalConflict(format!(
            "目标位置已存在 {}",
            to.display()
        ))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn matches_directory(from: &Path, _to: &Path, destination: &fs::Metadata) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let source = fs::metadata(from)?;
    Ok((source.dev(), source.ino()) == (destination.dev(), destination.ino()))
}

#[cfg(not(unix))]
fn matches_directory(from: &Path, to: &Path, _destination: &fs::Metadata) -> io::Result<bool> {
    Ok(to.canonicalize()? == from)
}

#[cfg(unix)]
pub(super) fn has_stored_name(path: &Path) -> Result<bool> {
    use crate::{domain::invalid, store::layout::parent};
    use unicode_normalization::UnicodeNormalization;

    let expected = path.file_name().ok_or_else(|| invalid("缺少工作区名称"))?;
    for entry in fs::read_dir(parent(path)?)? {
        let actual = entry?.file_name();
        if actual == expected
            || actual
                .to_str()
                .zip(expected.to_str())
                .is_some_and(|(actual, expected)| actual.nfc().eq(expected.nfc()))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
pub(super) fn has_stored_name(path: &Path) -> Result<bool> {
    Ok(path.canonicalize()? == path)
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
pub(super) use crate::store::layout::rename_new as rename_directory;

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
)))]
pub(super) fn rename_directory(from: &Path, to: &Path) -> io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "target directory exists",
        ));
    }
    fs::rename(from, to)
}
