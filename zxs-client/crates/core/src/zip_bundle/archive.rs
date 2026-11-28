use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek},
    iter::successors,
    path::Path,
};

use zip::ZipArchive;

use super::{
    header::{LENGTH, has_manifest},
    rules::Rules,
};
use crate::{
    domain::{BUFFER, EntryKind, LocalPath, METADATA_LIMIT, MemberHashes, Result, invalid},
    store::{
        digest,
        layout::{IGNORE_FILE, parent},
        space,
    },
};

mod browse;

pub use browse::LocalBundle;

const ENTRY_LIMIT: usize = 200_000;
const BYTE_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

pub struct Bundle {
    zip: ZipArchive<BufReader<File>>,
    members: BTreeMap<LocalPath, ArchiveMember>,
    rules: Rules,
    size: u64,
}

pub struct Expansion {
    pub members: BTreeMap<LocalPath, EntryKind>,
    pub hashes: MemberHashes,
}

struct ArchiveMember {
    kind: EntryKind,
    index: usize,
    size: u64,
}

impl Bundle {
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let mut file = File::open(path)?;
        let mut header = [0u8; LENGTH];
        match file.read_exact(&mut header) {
            Ok(()) if has_manifest(&header) => {}
            Ok(()) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let mut zip = ZipArchive::new(BufReader::with_capacity(64 * 1024, file))?;
        let Some(rules) = manifest(&mut zip)? else {
            return Ok(None);
        };
        let (mut members, size) = inventory(&mut zip)?;
        members.retain(|path, member| !rules.ignores(path, member.kind == EntryKind::Directory));
        Ok(Some(Self {
            zip,
            members,
            rules,
            size,
        }))
    }

    pub fn rules(&self) -> &Rules {
        &self.rules
    }

    pub fn extract(&mut self, stage: &Path) -> Result<Expansion> {
        space::require(stage, self.size)?;
        fs::create_dir(stage)?;
        let mut included = BTreeMap::new();
        let mut hashes = MemberHashes::new();
        for (path, source) in &self.members {
            // Only validated member paths are written to this new private directory.
            let target = stage.join(path.as_str());
            if source.kind == EntryKind::Directory {
                fs::create_dir_all(&target)?;
            } else {
                fs::create_dir_all(parent(&target)?)?;
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                let reader = self
                    .zip
                    .by_index(source.index)?
                    .take(source.size.saturating_add(1));
                let buffer = usize::try_from(source.size)
                    .unwrap_or(BUFFER)
                    .clamp(1, BUFFER);
                let (length, hash) = digest::copy(reader, &mut file, buffer)?;
                if length != source.size {
                    return Err(invalid(format!("invalid entry size: {}", path.as_str())));
                }
                file.sync_all()?;
                hashes.insert(path.clone(), hash);
            }
            included.insert(path.clone(), source.kind);
            for path in successors(path.parent(), LocalPath::parent).filter(|p| !p.is_root()) {
                included.entry(path).or_insert(EntryKind::Directory);
            }
        }
        Ok(Expansion {
            members: included,
            hashes,
        })
    }
}

fn manifest(zip: &mut ZipArchive<impl Read + Seek>) -> Result<Option<Rules>> {
    if zip.is_empty() {
        return Ok(None);
    }
    let file = zip.by_index(0)?;
    if file.name() != IGNORE_FILE
        || file.header_start() != 0
        || file.is_dir()
        || file.is_symlink()
        || file
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170_000, 0 | 0o100_000))
    {
        return Ok(None);
    }
    if file.size() > METADATA_LIMIT as u64 {
        return Err(invalid(".zxsignore exceeds 1 MiB"));
    }
    let mut text = Vec::new();
    file.take(METADATA_LIMIT as u64 + 1)
        .read_to_end(&mut text)?;
    if text.len() > METADATA_LIMIT {
        return Err(invalid(".zxsignore exceeds 1 MiB"));
    }
    Ok(Some(Rules::from_bytes(&text)?))
}

fn inventory(
    zip: &mut ZipArchive<BufReader<File>>,
) -> Result<(BTreeMap<LocalPath, ArchiveMember>, u64)> {
    if zip.len() > ENTRY_LIMIT {
        return Err(invalid("目录包超过 200,000 项"));
    }
    let limit = extraction_limit()?;
    let mut total = 0u64;
    let mut members = BTreeMap::new();
    for index in 0..zip.len() {
        let file = zip.by_index(index)?;
        let (path, member) = archive_member(&file, index)?;
        total = total
            .checked_add(member.size)
            .filter(|size| *size <= limit)
            .ok_or_else(|| invalid(format!("目录包解压体积超过 {limit} 字节")))?;
        if members.insert(path.clone(), member).is_some() {
            return Err(invalid(format!("duplicate ZIP path: {}", path.as_str())));
        }
    }
    validate_namespace(members.iter().map(|(path, member)| (path, member.kind)))?;
    Ok((members, total))
}

fn extraction_limit() -> Result<u64> {
    match std::env::var("ZXS_MAX_BUNDLE_BYTES") {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| invalid("ZXS_MAX_BUNDLE_BYTES must be a byte count")),
        Err(std::env::VarError::NotPresent) => Ok(BYTE_LIMIT),
        Err(error) => Err(invalid(error.to_string())),
    }
}

fn archive_member(
    file: &zip::read::ZipFile<'_, impl Read>,
    index: usize,
) -> Result<(LocalPath, ArchiveMember)> {
    let raw =
        std::str::from_utf8(file.name_raw()).map_err(|_| invalid("ZIP paths must be UTF-8"))?;
    let directory = file.is_dir();
    let path = member_path(raw, directory)?;
    let kind = if directory {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    if file.is_symlink() {
        return Err(invalid(format!("unsafe ZIP entry: {raw}")));
    }
    if file
        .unix_mode()
        .is_some_and(|mode| !matches!(mode & 0o170_000, 0 | 0o100_000 | 0o040_000))
    {
        return Err(invalid(format!("unsupported ZIP entry: {raw}")));
    }
    let member = ArchiveMember {
        index,
        size: file.size(),
        kind,
    };
    Ok((path, member))
}

fn member_path(raw: &str, directory: bool) -> Result<LocalPath> {
    let path = LocalPath::parse(if directory {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    })?;
    if path.is_root() {
        return Err(invalid(format!("unsafe ZIP entry: {raw}")));
    }
    Ok(path)
}

fn validate_namespace<'a>(
    members: impl Iterator<Item = (&'a LocalPath, EntryKind)> + Clone,
) -> Result<()> {
    let mut known = HashMap::new();
    for (path, kind) in members.clone() {
        if known.insert(path.key(), (path.clone(), kind)).is_some() {
            return Err(invalid(format!("duplicate ZIP path: {}", path.as_str())));
        }
    }
    for (path, _) in members {
        for path in successors(path.parent(), LocalPath::parent).filter(|p| !p.is_root()) {
            let (spelling, kind) = known
                .entry(path.key())
                .or_insert_with(|| (path.clone(), EntryKind::Directory));
            if spelling != &path || *kind != EntryKind::Directory {
                return Err(invalid(format!(
                    "conflicting ZIP directory: {}",
                    path.as_str()
                )));
            }
        }
    }
    Ok(())
}
