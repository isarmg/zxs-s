use std::{
    fs, io,
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

use crate::domain::{Error, LocalPath, Result, file_name_key, invalid};

pub fn check_unique_name(path: &Path) -> Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let key = file_name_key(name);
    find_name(super::parent(path)?, name, |other| {
        file_name_key(other) == key
    })?;
    Ok(())
}

pub fn checked_join(root: &Path, relative: &LocalPath) -> Result<PathBuf> {
    join_spelling(root, relative, relative.as_str(), 0)
}

pub(super) fn join_spelling(
    root: &Path,
    relative: &LocalPath,
    supplied: &str,
    folder_depth: usize,
) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    if relative.is_root() {
        return Ok(path);
    }
    let parts = relative.as_str().split('/').zip(supplied.split('/'));
    for (depth, (part, raw)) in parts.enumerate() {
        let directory = path.clone();
        let same_spelling = |name: &str| name.nfc().eq(part.chars());
        path.push(part);
        if part != raw
            && let Some(actual) = find_name(&directory, part, same_spelling)?
        {
            path = actual;
        }
        let file_type = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let key = (depth < folder_depth).then(|| file_name_key(part));
                let Some(actual) = find_name(&directory, part, |name| {
                    key.as_ref()
                        .map_or_else(|| same_spelling(name), |key| file_name_key(name) == *key)
                })?
                else {
                    continue;
                };
                if key.is_some()
                    && actual
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| !same_spelling(name))
                {
                    return Err(Error::LocalConflict(format!(
                        "本地路径「{}」与目标「{}」的大小写不一致",
                        actual.display(),
                        path.display()
                    )));
                }
                path = actual;
                fs::symlink_metadata(&path)?.file_type()
            }
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => continue,
            Err(error) => return Err(error.into()),
        };
        if file_type.is_symlink() {
            return Err(Error::SymbolicLink(path));
        }
    }
    Ok(path)
}

fn find_name(
    directory: &Path,
    part: &str,
    matches: impl Fn(&str) -> bool,
) -> Result<Option<PathBuf>> {
    let children = match fs::read_dir(directory) {
        Ok(children) => children,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let mut matching = None;
    for entry in children {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(&matches)
            && matching.replace(entry.path()).is_some()
        {
            return Err(invalid(format!("multiple canonical spellings of {part}")));
        }
    }
    Ok(matching)
}
