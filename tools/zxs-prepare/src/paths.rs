use std::{
    borrow::Cow,
    fs::{self, File, Metadata},
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::{Result, invalid};

pub const SYSTEM: &str = ".zxs";
pub const BUFFER: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Directory,
    File,
    Unsupported,
}

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub bytes: u64,
}

pub fn root(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(invalid(format!("请选择已有目录 {}", path.display())));
    }
    Ok(path)
}

pub fn entries(directory: &Path, skip_system: bool) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid(format!("目录含非 UTF-8 名称 {}", directory.display())))?;
        if skip_system && name == SYSTEM {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        entries.push(Entry {
            name,
            path,
            kind: kind(&metadata),
            bytes: metadata.len(),
        });
    }
    entries.sort_unstable_by(|left, right| (left.kind, &left.name).cmp(&(right.kind, &right.name)));
    Ok(entries)
}

pub fn kind(metadata: &Metadata) -> EntryKind {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & REPARSE_POINT != 0 {
            return EntryKind::Unsupported;
        }
    }
    if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Unsupported
    }
}

pub fn name(raw: &str) -> Result<String> {
    let value: String = raw.nfc().collect();
    if (1..=255).contains(&value.len())
        && !value.ends_with(['.', ' '])
        && !value.eq_ignore_ascii_case(SYSTEM)
        && !value.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
        && !device_name(&value)
    {
        Ok(value)
    } else {
        Err(invalid(format!("名称无法在 ZXS 中使用 {raw:?}")))
    }
}

fn device_name(value: &str) -> bool {
    let base = value
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(
                    number,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
}

pub fn key(name: &str) -> String {
    name.to_lowercase().nfc().collect()
}

pub fn child(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

pub fn slash_path(path: &str) -> Cow<'_, str> {
    if cfg!(windows) && path.contains('\\') {
        Cow::Owned(path.replace('\\', "/"))
    } else {
        Cow::Borrowed(path)
    }
}

pub fn without_trailing_separator(path: &str) -> &str {
    path.strip_suffix('/')
        .filter(|relative| !relative.is_empty())
        .unwrap_or(path)
}

pub fn within(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_owned();
    if !relative.is_empty() {
        for part in relative.split('/') {
            name(part)?;
            path.push(part);
            if kind(&fs::symlink_metadata(&path)?) != EntryKind::Directory {
                return Err(invalid(format!("无法进入目录 {}", path.display())));
            }
        }
    }
    Ok(path)
}

pub fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn mkdir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && kind(&fs::symlink_metadata(path)?) == EntryKind::Directory =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg_attr(
    windows,
    allow(
        clippy::unnecessary_wraps,
        reason = "the Windows standard library cannot open directory handles for sync"
    )
)]
pub fn sync_dir(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

pub fn mark_readonly(file: &File) -> Result<()> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(0o444)
    };
    #[cfg(windows)]
    let permissions = {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        permissions
    };
    file.set_permissions(permissions)?;
    file.sync_all()?;
    Ok(())
}

pub fn rename_new(source: &Path, target: &Path) -> Result<()> {
    if exists(target)? {
        return Err(invalid(format!("目标已存在 {}", target.display())));
    }
    fs::rename(source, target)?;
    Ok(())
}

pub fn copy_file(source: &Entry, mut output: impl Write) -> Result<()> {
    let path = &source.path;
    let before = fs::symlink_metadata(path)?;
    if kind(&before) != EntryKind::File {
        return Err(invalid(format!("不是普通文件 {}", path.display())));
    }
    let changed = || invalid(format!("准备期间文件发生变更 {}", path.display()));
    if before.len() != source.bytes {
        return Err(changed());
    }
    let mut input = BufReader::with_capacity(BUFFER, File::open(path)?);
    let bytes = io::copy(&mut input, &mut output)?;
    let after = fs::symlink_metadata(path)?;
    if bytes != source.bytes
        || after.len() != source.bytes
        || before.modified()? != after.modified()?
    {
        return Err(changed());
    }
    Ok(())
}
