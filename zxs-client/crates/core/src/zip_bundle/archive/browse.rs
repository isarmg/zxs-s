use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read},
    path::Path,
};

use serde::Serialize;
use zip::read::ZipFile;

use super::{ArchiveMember, Bundle};
use crate::{
    domain::{
        ContainerId, EntryKind, Error, FileName, LocalPath, Result, Version, VersionNum, invalid,
    },
    store::space,
};

const PAGE_SIZE: usize = 100;

pub struct LocalBundle {
    archive: Bundle,
    children: BTreeMap<LocalPath, Vec<BundleEntry>>,
    id: ContainerId,
    version: Version,
}

#[derive(Serialize)]
pub struct BundlePage {
    pub version: VersionNum,
    pub entries: Vec<BundleEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct BundleEntry {
    pub path: LocalPath,
    pub name: FileName,
    pub kind: EntryKind,
}

pub struct BundleMember<'a> {
    file: ZipFile<'a, BufReader<File>>,
    pub name: FileName,
    pub size: u64,
    pub cache_key: String,
}

impl LocalBundle {
    /// Opens a downloaded ZIP workspace and builds its visible directory tree.
    ///
    /// # Errors
    /// Returns an error if the archive cannot be opened or its workspace structure is invalid.
    pub fn open(path: &Path, id: ContainerId, version: Version) -> Result<Option<Self>> {
        let Some(archive) = Bundle::open(path)? else {
            return Ok(None);
        };
        let mut entries = BTreeMap::new();
        for (path, member) in &archive.members {
            if archive
                .rules
                .ignores(path, member.kind == EntryKind::Directory)
            {
                continue;
            }
            entries.insert(path.clone(), member.kind);
            let mut ancestor = path.parent();
            while let Some(path) = ancestor.filter(|path| !path.is_root()) {
                ancestor = path.parent();
                if entries.insert(path, EntryKind::Directory).is_some() {
                    break;
                }
            }
        }
        let mut children = BTreeMap::<_, Vec<_>>::from([(LocalPath::parse(".")?, Vec::new())]);
        for (path, kind) in entries {
            if kind == EntryKind::Directory {
                children.entry(path.clone()).or_default();
            }
            if let Some(parent) = path.parent() {
                let name = FileName::parse(path.name())?;
                children
                    .entry(parent)
                    .or_default()
                    .push(BundleEntry { path, name, kind });
            }
        }
        for siblings in children.values_mut() {
            siblings.sort_by(|left, right| {
                (left.kind != EntryKind::Directory, &left.path)
                    .cmp(&(right.kind != EntryKind::Directory, &right.path))
            });
        }
        Ok(Some(Self {
            archive,
            children,
            id,
            version,
        }))
    }

    /// Lists immediate children, including explicit empty and implicit directories.
    ///
    /// # Errors
    /// Returns an error for an unknown directory or invalid cursor.
    pub fn browse(&self, directory: &LocalPath, cursor: Option<&str>) -> Result<BundlePage> {
        let children = self
            .children
            .get(directory)
            .ok_or_else(|| missing(directory))?;
        let skip = cursor
            .map_or(Ok(0), str::parse::<usize>)
            .map_err(|_| invalid("无效的目录工作区游标"))?;
        if skip > children.len() {
            return Err(invalid("无效的目录工作区游标"));
        }
        let end = skip.saturating_add(PAGE_SIZE).min(children.len());
        Ok(BundlePage {
            version: self.version.version,
            entries: children[skip..end].to_vec(),
            next_cursor: (end < children.len()).then(|| end.to_string()),
        })
    }

    /// Opens a visible file from the local archive.
    ///
    /// # Errors
    /// Returns an error for directories, unknown members, or unreadable ZIP data.
    pub fn member(&mut self, path: &LocalPath) -> Result<BundleMember<'_>> {
        let member = self.file_member(path)?;
        let key = blake3::hash(format!("{}{}", self.version.blake3, path.as_str()).as_bytes());
        let cache_key = format!("{}-{}-{key}", self.id, self.version.version);
        Ok(BundleMember {
            file: self.archive.zip.by_index(member.index)?,
            name: FileName::parse(path.name())?,
            size: member.size,
            cache_key,
        })
    }

    fn file_member(&self, path: &LocalPath) -> Result<ArchiveMember> {
        self.archive
            .members
            .get(path)
            .filter(|member| {
                member.kind == EntryKind::File && !self.archive.rules.ignores(path, false)
            })
            .copied()
            .ok_or_else(|| missing(path))
    }
}

impl BundleMember<'_> {
    /// Extracts the selected member to a new file and verifies its ZIP size and checksum.
    ///
    /// # Errors
    /// Returns an error if the destination exists or extraction fails validation.
    pub fn write_new(self, path: &Path) -> Result<()> {
        space::require(path, self.size)?;
        let mut output = OpenOptions::new().create_new(true).write(true).open(path)?;
        let mut input = self.file.take(self.size.saturating_add(1));
        let result = (|| {
            if io::copy(&mut input, &mut output)? != self.size {
                return Err(invalid("目录工作区成员长度不匹配"));
            }
            output.sync_all()?;
            Ok(())
        })();
        drop(output);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }
}

fn missing(path: &LocalPath) -> Error {
    Error::Remote {
        status: 404,
        message: format!("目录工作区找不到 {}", path.as_str()),
    }
}
