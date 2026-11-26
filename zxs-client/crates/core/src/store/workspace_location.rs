use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    layout::read_small,
    profile::{directory, save_metadata},
};
use crate::domain::{Error, Result, invalid};

const SETTINGS: &str = "workspace-location.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Location {
    path: PathBuf,
}

/// Reads the selected workspace path.
/// # Errors
/// Returns an error if the setting cannot be read or decoded.
pub fn configured_workspace() -> Result<Option<PathBuf>> {
    match read_small(&directory()?.join(SETTINGS)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice::<Location>(&bytes)?.path)),
        Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Selects an existing directory for subsequent workspace operations.
/// # Errors
/// Returns an error if the directory or setting cannot be opened.
pub fn select_workspace(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("请输入工作区目录的绝对路径"));
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(invalid("工作区须是已有文件夹"));
    }
    let location = Location { path };
    save_metadata(&directory()?, SETTINGS, &location)?;
    Ok(location.path)
}
