use std::{
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    layout::{
        Layout, TempDir, WORKSPACE_DIR, WORKSPACE_MOVE_MARKER, create_private, lock_file, parent,
        read_small, sync_parent,
    },
    profile::{directory, save_metadata},
    random_hex,
};
use crate::domain::{Error, FileName, Result, TxId, invalid};

use self::paths::{has_stored_name, rename_directory, same_directory};

mod copy_tree;
mod paths;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
mod windows_tests;

const SETTINGS: &str = "workspace-location.json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Location {
    Active(Active),
    Moving(Move),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    path: PathBuf,
    marker: Option<TxId>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    from: PathBuf,
    to: PathBuf,
    intermediate: Option<PathBuf>,
    id: TxId,
}

pub struct WorkspaceMove {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// Loads the saved local workspace location and completes recovery if a move was interrupted.
///
/// # Errors
/// Returns an error if the saved setting is invalid or an interrupted move cannot be recovered.
pub fn configured_workspace() -> Result<Option<PathBuf>> {
    configured_workspace_in(&directory()?)
}

fn configured_workspace_in(config: &Path) -> Result<Option<PathBuf>> {
    match read_location(config)? {
        Some(Location::Active(active)) if !active.has_marker()? => Ok(Some(active.path)),
        Some(_) => {
            let _lock = settings_lock(config)?;
            Ok(recover_location(config)?.map(|active| active.path))
        }
        None => Ok(None),
    }
}
/// Moves the workspace directory and saves the result for subsequent client launches.
///
/// # Errors
/// Returns an error if the origin or destination is invalid or the files cannot be moved.
/// Warnings report cleanup or settings errors after the move has already taken effect.
pub fn move_workspace(root: &Path, directory: &Path, name: &FileName) -> Result<WorkspaceMove> {
    move_workspace_in(&self::directory()?, root, directory, name)
}

fn move_workspace_in(
    config: &Path,
    root: &Path,
    destination_parent: &Path,
    name: &FileName,
) -> Result<WorkspaceMove> {
    if !destination_parent.is_absolute() {
        return Err(invalid("请输入工作区所在文件夹的绝对路径"));
    }
    let destination_parent = destination_parent.canonicalize()?;
    if !destination_parent.is_dir() {
        return Err(invalid("新位置须是文件夹"));
    }
    let from = root.canonicalize()?;
    let source_parent = parent(&from)?;
    if destination_parent.starts_with(&from) {
        return Err(invalid("不能把工作区移入它自己的子目录"));
    }
    let to = destination_parent.join(name.as_str());
    let _settings_lock = settings_lock(config)?;
    if config.canonicalize()?.starts_with(&from) {
        return Err(invalid(
            "客户端设置目录在工作区内，请先把设置目录放在工作区之外",
        ));
    }
    let previous = recover_location(config)?;
    let (_layout, workspace_lock) = Layout::open(&from)?;

    if from == to {
        let active = Active {
            path: to.clone(),
            marker: None,
        };
        save_metadata(config, SETTINGS, &Location::Active(active))?;
        return Ok(WorkspaceMove {
            path: to,
            warnings: Vec::new(),
        });
    }
    let mut case_stage = same_directory(&from, &to)?
        .then(|| TempDir::create(source_parent))
        .transpose()?;
    let movement = Move {
        from,
        to,
        intermediate: case_stage.as_ref().map(|stage| stage.child("workspace")),
        id: TxId::parse(random_hex::<16>()?)?,
    };
    save_metadata(config, SETTINGS, &Location::Moving(movement.clone()))?;
    movement.mark_origin()?;
    if let Some(stage) = &mut case_stage {
        stage.preserve();
    }
    // An open file handle can prevent Windows from moving the directory. The
    // durable marker blocks workspace operations until we can finalize the move.
    drop(workspace_lock);
    match movement.transfer() {
        Ok(copied) => Ok(movement.finish(config, copied)),
        Err(error) => {
            movement.restore()?;
            remove_marker(&movement.from, &movement.id)?;
            if let Some(active) = previous {
                save_metadata(config, SETTINGS, &Location::Active(active))?;
            } else {
                let path = config.join(SETTINGS);
                fs::remove_file(&path)?;
                sync_parent(&path)?;
            }
            Err(error)
        }
    }
}

impl Move {
    fn mark_origin(&self) -> Result<()> {
        save_metadata(
            &self.from.join(WORKSPACE_DIR),
            WORKSPACE_MOVE_MARKER,
            &self.id,
        )
    }

    fn transfer(&self) -> Result<bool> {
        if let Some(intermediate) = &self.intermediate {
            rename_directory(&self.from, intermediate)?;
            rename_directory(intermediate, &self.to)?;
            self.remove_intermediate();
            return Ok(false);
        }
        match rename_directory(&self.from, &self.to) {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == ErrorKind::CrossesDevices => {
                let temporary = TempDir::create(parent(&self.to)?)?;
                let contents = temporary.child("workspace");
                copy_tree::copy(&self.from, &contents)?;
                copy_tree::verify(&self.from, &contents)?;
                rename_directory(&contents, &self.to)?;
                Ok(true)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn restore(&self) -> Result<()> {
        if let Some(intermediate) = &self.intermediate
            && read_marker(intermediate)?.as_ref() == Some(&self.id)
        {
            rename_directory(intermediate, &self.from).map_err(|error| {
                Error::LocalConflict(format!(
                    "无法恢复原工作区，文件保留在 {} · {error}",
                    intermediate.display()
                ))
            })?;
            sync_parent(&self.from)?;
        }
        self.remove_intermediate();
        Ok(())
    }

    fn remove_intermediate(&self) {
        if let Some(directory) = self.intermediate.as_deref().and_then(Path::parent) {
            let _ = fs::remove_dir(directory);
        }
    }

    fn finish(self, config: &Path, copied: bool) -> WorkspaceMove {
        let mut warnings = Vec::new();
        if let Err(error) = sync_parent(&self.from).and_then(|()| sync_parent(&self.to)) {
            warnings.push(format!("已移动工作区，目录写入确认失败 · {error}"));
            return WorkspaceMove {
                path: self.to,
                warnings,
            };
        }
        if copied && let Err(error) = self.retire_source() {
            warnings.push(format!("新工作区已启用，清理旧位置遇到错误 · {error}"));
            if let Err(error) = remove_marker(&self.from, &self.id) {
                warnings.push(format!("旧工作区的迁移标记将在下次启动时清理 · {error}"));
                return WorkspaceMove {
                    path: self.to,
                    warnings,
                };
            }
        }
        if let Err(error) = self.activate(config, self.to.clone()) {
            warnings.push(format!(
                "已移动工作区，下次启动将通过迁移记录恢复位置 · {error}"
            ));
        }
        WorkspaceMove {
            path: self.to,
            warnings,
        }
    }

    fn retire_source(&self) -> Result<()> {
        // A file edited outside ZXS after the copy must stay recoverable.
        copy_tree::verify(&self.from, &self.to)?;
        let mut retired = TempDir::create(parent(&self.from)?)?;
        let contents = retired.child("workspace");
        rename_directory(&self.from, &contents)?;
        retired.preserve();
        remove_marker(&contents, &self.id)?;
        fs::remove_dir_all(&contents).map_err(|error| {
            Error::LocalConflict(format!("旧文件保留在 {} · {error}", contents.display()))
        })?;
        fs::remove_dir(parent(&contents)?)?;
        sync_parent(&self.from)
    }

    fn recover(&self) -> Result<PathBuf> {
        // A case-insensitive filesystem can resolve the new name before the rename.
        if read_marker(&self.to)?.as_ref() == Some(&self.id) && has_stored_name(&self.to)? {
            if self.intermediate.is_none() {
                remove_marker(&self.from, &self.id)?;
            }
            self.remove_intermediate();
            return Ok(self.to.clone());
        }
        self.restore()?;
        if fs::metadata(&self.from).is_ok_and(|entry| entry.is_dir()) {
            return Ok(self.from.clone());
        }
        Err(invalid(format!(
            "找不到迁移中的工作区，请检查 {} 和 {}",
            self.from.display(),
            self.to.display()
        )))
    }

    fn activate(&self, config: &Path, path: PathBuf) -> Result<Active> {
        let active = Active {
            path,
            marker: Some(self.id.clone()),
        };
        save_metadata(config, SETTINGS, &Location::Active(active.clone()))?;
        remove_marker(&active.path, &self.id)?;
        Ok(active)
    }
}

impl Active {
    fn has_marker(&self) -> Result<bool> {
        if let Some(id) = &self.marker {
            return Ok(read_marker(&self.path)?.as_ref() == Some(id));
        }
        Ok(false)
    }
}

fn marker_path(path: &Path) -> PathBuf {
    path.join(WORKSPACE_DIR).join(WORKSPACE_MOVE_MARKER)
}

fn read_marker(path: &Path) -> Result<Option<TxId>> {
    read_setting(&marker_path(path))
}

fn remove_marker(path: &Path, id: &TxId) -> Result<()> {
    if read_marker(path)?.as_ref() == Some(id) {
        let marker = marker_path(path);
        fs::remove_file(&marker)?;
        sync_parent(&marker)?;
    }
    Ok(())
}

fn settings_lock(config: &Path) -> Result<File> {
    fs::create_dir_all(parent(config)?)?;
    create_private(config)?;
    lock_file(&config.join("workspace-location.lock"))
}

fn read_location(config: &Path) -> Result<Option<Location>> {
    read_setting(&config.join(SETTINGS))
}

fn read_setting<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match read_small(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn recover_location(config: &Path) -> Result<Option<Active>> {
    match read_location(config)? {
        Some(Location::Active(active)) => {
            if let Some(id) = &active.marker {
                remove_marker(&active.path, id)?;
            }
            Ok(Some(active))
        }
        Some(Location::Moving(movement)) => {
            let path = movement.recover()?;
            Ok(Some(movement.activate(config, path)?))
        }
        None => Ok(None),
    }
}
