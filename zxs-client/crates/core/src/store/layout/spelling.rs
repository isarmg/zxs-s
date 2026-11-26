use std::{
    fs, io,
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

use crate::domain::{Error, LocalPath, Result, invalid};

pub fn check_unique_name(path: &Path) -> Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let key = name.to_lowercase().nfc().collect::<String>();
    find_name(super::parent(path)?, name, |other| {
        other.to_lowercase().nfc().eq(key.chars())
    })?;
    Ok(())
}

pub fn checked_join(root: &Path, relative: &LocalPath) -> Result<PathBuf> {
    join_spelling(root, relative, relative.as_str())
}

pub(super) fn join_spelling(root: &Path, relative: &LocalPath, supplied: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    if relative.is_root() {
        return Ok(path);
    }
    for (part, raw) in relative.as_str().split('/').zip(supplied.split('/')) {
        let directory = path.clone();
        path.push(part);
        if part != raw
            && let Some(actual) = find_name(&directory, part, |name| name.nfc().eq(part.chars()))?
        {
            path = actual;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::SymbolicLink(path));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(actual) =
                    find_name(&directory, part, |name| name.nfc().eq(part.chars()))?
                {
                    if fs::symlink_metadata(&actual)?.file_type().is_symlink() {
                        return Err(Error::SymbolicLink(actual));
                    }
                    path = actual;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {}
            Err(error) => return Err(error.into()),
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
        if entry.file_name().to_str().is_some_and(&matches) {
            if matching.is_some() {
                return Err(invalid(format!("multiple canonical spellings of {part}")));
            }
            matching = Some(entry.path());
        }
    }
    Ok(matching)
}
