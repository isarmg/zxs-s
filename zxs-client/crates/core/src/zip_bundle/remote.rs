mod ranges;

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Read},
    path::Path,
    rc::Rc,
};

use serde::Serialize;
use zip::{
    ZipArchive,
    read::{ArchiveOffset, Config, ZipFile},
    result::ZipError,
};

use super::{
    archive::{
        ENTRY_LIMIT, archive_member, extraction_limit, manifest, member_path, validate_namespace,
    },
    rules::Rules,
    zip_stem,
};
use crate::{
    domain::{
        ContainerId, EntryKind, Error, FileName, LocalPath, Result, Version, VersionNum, invalid,
    },
    store::space,
    transport::Remote,
};

use ranges::PayloadRanges;

const PAGE_SIZE: usize = 100;

pub struct RemoteBundle {
    archive: ZipArchive<PayloadRanges>,
    entries: BTreeMap<LocalPath, Entry>,
    id: ContainerId,
    version: Version,
    member_end: Rc<Cell<Option<u64>>>,
}

#[derive(Clone, Copy)]
struct Entry {
    kind: EntryKind,
    index: usize,
}

#[derive(Serialize)]
pub struct BundlePage {
    pub version: VersionNum,
    pub entries: Vec<BundleEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize)]
pub struct BundleEntry {
    pub path: LocalPath,
    pub name: FileName,
    pub kind: EntryKind,
}

pub struct BundleMember<'a> {
    file: ZipFile<'a, PayloadRanges>,
    pub name: FileName,
    pub size: u64,
    pub cache_key: String,
}

impl RemoteBundle {
    /// Opens a fixed archive version over authenticated partial payload requests.
    /// # Errors
    /// Returns an error if the version is unavailable, malformed, or not a ZXS workspace.
    pub fn open(remote: &Remote, id: &ContainerId, version: Option<VersionNum>) -> Result<Self> {
        let (container, version) = remote.version(id, version)?;
        if zip_stem(container.name.as_str()).is_none()
            || !remote.probe_bundle(id, version.version)?
        {
            return Err(invalid("该版本不是目录工作区"));
        }
        let member_end = Rc::default();
        let ranges = PayloadRanges::new(remote, id, version.clone(), Rc::clone(&member_end));
        let config = Config {
            archive_offset: ArchiveOffset::Known(0),
        };
        let mut archive = ZipArchive::with_config(config, ranges)
            .map_err(|error| transport_error(error.into()))?;
        let rules = manifest(&mut archive)
            .map_err(transport_error)?
            .ok_or_else(|| invalid("目录工作区缺少有效的 .zxsignore"))?;
        let entries = visible_entries(&archive, &rules)?;
        Ok(Self {
            archive,
            entries,
            id: id.clone(),
            version,
            member_end,
        })
    }

    /// Lists immediate children, including explicit empty and implicit directories.
    /// # Errors
    /// Returns an error for an unknown directory or invalid cursor.
    pub fn browse(&self, directory: &LocalPath, cursor: Option<&str>) -> Result<BundlePage> {
        if !directory.is_root()
            && self.entries.get(directory).map(|entry| entry.kind) != Some(EntryKind::Directory)
        {
            return Err(missing(directory));
        }
        let skip = cursor
            .map_or(Ok(0), str::parse::<usize>)
            .map_err(|_| invalid("无效的目录工作区游标"))?;
        let mut children = self
            .entries
            .iter()
            .filter(|(path, _)| path.parent().as_ref() == Some(directory))
            .collect::<Vec<_>>();
        children.sort_unstable_by_key(|(path, entry)| (entry.kind != EntryKind::Directory, *path));
        if skip > children.len() {
            return Err(invalid("无效的目录工作区游标"));
        }
        let end = skip.saturating_add(PAGE_SIZE).min(children.len());
        let entries = children[skip..end]
            .iter()
            .map(|(path, entry)| {
                Ok(BundleEntry {
                    path: (*path).clone(),
                    name: leaf_name(path)?,
                    kind: entry.kind,
                })
            })
            .collect::<Result<_>>()?;
        Ok(BundlePage {
            version: self.version.version,
            entries,
            next_cursor: (end < children.len()).then(|| end.to_string()),
        })
    }

    /// Opens one member for extraction while the rest of the archive remains remote.
    /// # Errors
    /// Returns an error for directories, unknown members, or invalid file metadata.
    pub fn member(&mut self, path: &LocalPath) -> Result<BundleMember<'_>> {
        let entry = self
            .entries
            .get(path)
            .filter(|entry| entry.kind == EntryKind::File)
            .ok_or_else(|| missing(path))?;
        let key = blake3::hash(format!("{}{}", self.version.blake3, path.as_str()).as_bytes());
        let cache_key = format!("{}-{}-{key}", self.id, self.version.version);
        let central_start = self.archive.central_directory_start();
        self.member_end.set(None);
        let file = self
            .archive
            .by_index(entry.index)
            .map_err(|error| transport_error(error.into()))?;
        let (actual, metadata) = archive_member(&file, entry.index)?;
        if actual != *path || metadata.kind != EntryKind::File {
            return Err(invalid("目录工作区成员与索引不一致"));
        }
        let size = file.size();
        if size > extraction_limit()? {
            return Err(invalid("目录工作区成员超过解压体积限制"));
        }
        let end = file
            .data_start()
            .and_then(|start| start.checked_add(file.compressed_size()))
            .filter(|end| *end <= central_start)
            .ok_or_else(|| invalid("目录工作区成员的压缩数据位置无效"))?;
        self.member_end.set(Some(end));
        Ok(BundleMember {
            file,
            name: leaf_name(path)?,
            size,
            cache_key,
        })
    }
}

impl BundleMember<'_> {
    /// Extracts the selected member to a new file and verifies its ZIP size and checksum.
    /// # Errors
    /// Returns an error if the destination exists or extraction fails validation.
    pub fn write_new(self, path: &Path) -> Result<()> {
        space::require(path, self.size)?;
        let mut output = OpenOptions::new().create_new(true).write(true).open(path)?;
        let mut input = self.file.take(self.size.saturating_add(1));
        let result = (|| {
            let copied = io::copy(&mut input, &mut output)?;
            if copied != self.size {
                return Err(invalid("目录工作区成员长度不匹配"));
            }
            output.sync_all()?;
            Ok(())
        })();
        drop(output);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result.map_err(transport_error)
    }
}

fn visible_entries(
    archive: &ZipArchive<PayloadRanges>,
    rules: &Rules,
) -> Result<BTreeMap<LocalPath, Entry>> {
    if archive.len() > ENTRY_LIMIT {
        return Err(invalid("目录包超过 200,000 项"));
    }
    // The central directory provides the tree without reading the local headers
    // or compressed contents of files the user has not selected.
    let mut entries = BTreeMap::new();
    for (index, name) in archive.file_names().enumerate() {
        let directory = name.ends_with('/');
        let path = member_path(name, directory)?;
        let kind = if directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        if entries
            .insert(path.clone(), Entry { kind, index })
            .is_some()
        {
            return Err(invalid(format!("duplicate ZIP path: {}", path.as_str())));
        }
    }
    validate_namespace(entries.iter().map(|(path, entry)| (path, entry.kind)))?;
    entries.retain(|path, entry| !rules.ignores(path, entry.kind == EntryKind::Directory));
    let parents = entries
        .keys()
        .flat_map(|path| std::iter::successors(path.parent(), LocalPath::parent))
        .filter(|path| !path.is_root())
        .collect::<BTreeSet<_>>();
    for path in parents {
        entries.entry(path).or_insert(Entry {
            kind: EntryKind::Directory,
            index: 0,
        });
    }
    Ok(entries)
}

fn leaf_name(path: &LocalPath) -> Result<FileName> {
    FileName::parse(path.as_str().rsplit('/').next().unwrap_or_default())
}

fn missing(path: &LocalPath) -> Error {
    Error::Remote {
        status: 404,
        message: format!("目录工作区找不到 {}", path.as_str()),
    }
}

// ZIP decoding accepts std::io errors; restore any typed HTTP error afterwards.
fn transport_error(error: Error) -> Error {
    match error {
        Error::Io(error) | Error::Zip(ZipError::Io(error)) => {
            error.downcast::<Error>().unwrap_or_else(Error::Io)
        }
        error => error,
    }
}
