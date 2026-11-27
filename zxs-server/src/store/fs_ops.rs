use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{Layout, RegisteredDisk};
use crate::domain::{
    Container, ContainerMeta, Detail, DiskName, Error, FileName, METADATA_LIMIT, Pending, Result,
    TxId, Version, VersionNum, invalid, normalize_tags,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub listen: SocketAddr,
    pub recycle_dir: PathBuf,
    pub password_hash: String,
    pub data_disks: Vec<RegisteredDisk>,
    pub recycle_disks: Vec<RegisteredDisk>,
}

#[derive(Clone)]
pub struct Store {
    pub paths: Layout,
}

impl Store {
    pub fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.paths.lock())?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => Err(Error::Conflict(
                "storage is locked by another process".into(),
            )),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub fn prepare(&self) -> Result<()> {
        mkdir(&self.paths.system())?;
        mkdir(&self.paths.inflight())?;
        for path in self.paths.spools() {
            mkdir(&path)?;
        }
        Ok(())
    }

    pub fn config(&self) -> Result<Config> {
        toml::from_str(
            std::str::from_utf8(&small(&self.paths.config())?)
                .map_err(|err| Error::Config(err.to_string()))?,
        )
        .map_err(|err| Error::Config(err.to_string()))
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        let text = toml::to_string(config).map_err(|err| Error::Config(err.to_string()))?;
        atomic_bytes(&self.paths.config(), text.as_bytes())
    }

    pub fn stage(&self, disk: &DiskName, tx: &TxId) -> Result<StageGuard> {
        StageGuard::create(self.paths.staging(disk, tx))
    }

    pub fn stage_upload(&self, disk: &DiskName, tx: &TxId) -> Result<StageGuard> {
        let stage = self.stage(disk, tx)?;
        mkdir(&Layout::staged_version(&stage.path))?;
        Ok(stage)
    }

    pub fn stage_recycle(&self, disk: &DiskName, tx: &TxId) -> Result<StageGuard> {
        StageGuard::create(self.paths.incoming_operation(disk, tx))
    }

    pub fn stage_plan(&self, tx: &TxId) -> Result<StageGuard> {
        StageGuard::create(self.paths.tree_archives(tx))
    }

    pub fn discard_index(&self) -> Result<()> {
        for suffix in ["wal", "shm"] {
            remove_file(&self.paths.index_sidecar(suffix))?;
        }
        remove_file(&self.paths.index())
    }

    pub fn install_index(&self, index: &Path) -> Result<()> {
        rename(index, &self.paths.index())
    }

    pub fn start(&self, pending: &Pending) -> Result<()> {
        let path = self.journal(pending);
        if path.try_exists()? {
            return Err(Error::Recovery);
        }
        atomic_json(&path, pending)
    }

    pub fn journal(&self, pending: &Pending) -> PathBuf {
        self.paths.pending(&pending.tx_id)
    }

    pub fn end(&self, pending: &Pending) -> Result<()> {
        remove_file(&self.journal(pending))
    }

    pub fn snapshot(&self, path: &Path) -> Result<Detail> {
        let (folder, name) = self.paths.container_location(path)?;
        let meta = container_meta(path)?;
        let container = Container::at(folder, name, meta.clone());
        versions_at(path, container, &meta)
    }

    pub fn versions_at(path: &Path, container: Container) -> Result<Detail> {
        versions_at(path, container, &container_meta(path)?)
    }
}

fn versions_at(path: &Path, mut container: Container, meta: &ContainerMeta) -> Result<Detail> {
    container.tags = normalize_tags(container.tags);
    let versions = super::integrity::read_versions(path, meta)?;
    container.latest_blake3 = versions.last().map(|version| version.blake3.clone());
    Ok(Detail {
        container,
        versions,
    })
}

pub(super) fn container_meta(path: &Path) -> Result<ContainerMeta> {
    let metadata = Layout::metadata(path);
    let meta: ContainerMeta = read_json(&metadata)
        .map_err(|error| invalid(format!("{}: {error}", metadata.display())))?;
    if meta.first_retained_version > meta.latest_version {
        return Err(Error::IncompleteHistory(path.into()));
    }
    Ok(meta)
}

pub(super) fn read_version_files(path: &Path) -> Result<Vec<Version>> {
    let metadata = Layout::metadata(path);
    let mut versions = Vec::new();
    for dir in entries(path)? {
        if dir == metadata {
            continue;
        }
        let number = version_dir(&dir)
            .ok_or_else(|| invalid(format!("unexpected entry {}", dir.display())))?;
        let version = validate_version(&dir, number)
            .map_err(|error| invalid(format!("{}: {error}", dir.display())))?;
        versions.push(version);
    }
    versions.sort_unstable_by_key(|version| version.number);
    if versions.is_empty() {
        return Err(Error::DamagedContainer(path.into()));
    }
    Ok(versions)
}

fn validate_version(dir: &Path, number: VersionNum) -> Result<Version> {
    if Layout::version(parent(dir)?, number) != dir || !fs::symlink_metadata(dir)?.is_dir() {
        return Err(invalid("invalid version directory"));
    }
    let version: Version = read_json(&Layout::metadata(dir))?;
    let payload = Layout::payload_file(dir, &version.name_at_commit);
    let metadata = fs::symlink_metadata(payload)?;
    if version.number != number
        || !metadata.is_file()
        || metadata.len() != version.size
        || metadata.permissions().mode() & 0o777 != 0o444
    {
        return Err(invalid(
            "invalid version metadata, original size or permissions",
        ));
    }
    Ok(version)
}

pub struct StageGuard {
    pub path: PathBuf,
    status: StageStatus,
}

enum StageStatus {
    Uncommitted,
    Journaled,
}

impl StageGuard {
    fn create(path: PathBuf) -> Result<Self> {
        fs::create_dir(&path)?;
        let guard = Self {
            path,
            status: StageStatus::Uncommitted,
        };
        sync_parent(&guard.path)?;
        Ok(guard)
    }

    pub fn payload(&self, name: &FileName) -> PathBuf {
        Layout::payload_file(&Layout::staged_version(&self.path), name)
    }
    pub fn save_version(&self, version: &Version) -> Result<()> {
        let directory = Layout::staged_version(&self.path);
        // Publish the prepared directory as a unit, so a temporary file need not share the
        // namespace of the original payload.
        write_metadata(&Layout::metadata(&directory), &serde_json::to_vec(version)?)?;
        sync(&directory)
    }
    pub fn journaled(&mut self) {
        self.status = StageStatus::Journaled;
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if matches!(self.status, StageStatus::Uncommitted)
            && let Err(error) = remove_dir(&self.path)
        {
            tracing::error!(?error, "staging cleanup failed");
        }
    }
}

pub fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| invalid(format!("no parent for {}", path.display())))
}

pub fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    fs::read_dir(path)?.map(|entry| Ok(entry?.path())).collect()
}

pub fn version_dir(path: &Path) -> Option<VersionNum> {
    let value = path.file_name()?.to_str()?.strip_prefix('v')?;
    let version = VersionNum::new(value.parse().ok()?).ok()?;
    (value == format!("{:06}", version.get())).then_some(version)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = small(path)?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(format!("{}: {error}", path.display())))
}

pub(super) fn small(path: &Path) -> Result<Vec<u8>> {
    if !fs::symlink_metadata(parent(path)?)?.is_dir() || !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid(format!(
            "expected a regular metadata file: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(METADATA_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > METADATA_LIMIT {
        return Err(invalid(format!(
            "metadata exceeds 1 MiB: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?)
}

pub(super) fn atomic_bytes(path: &Path, value: &[u8]) -> Result<()> {
    let temp = Layout::temporary(path);
    write_metadata(&temp, value)?;
    rename(&temp, path)
}

fn write_metadata(path: &Path, value: &[u8]) -> Result<()> {
    if value.len() > METADATA_LIMIT {
        return Err(invalid("metadata exceeds 1 MiB"));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value)?;
    file.sync_all()?;
    Ok(())
}

pub fn mkdir(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = parent(path)?;
    mkdir(parent)?;
    match fs::create_dir(path) {
        Ok(()) => sync(parent),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(err) => Err(err.into()),
    }
}

pub fn sync(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub fn sync_parent(path: &Path) -> Result<()> {
    sync(parent(path)?)
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    sync_parent(to)?;
    if from.parent() != to.parent() {
        sync_parent(from)?;
    }
    Ok(())
}

pub fn remove_dir(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => sync_parent(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

pub fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
