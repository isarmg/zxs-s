use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::Layout;
use crate::domain::{
    Blake3Hash, Container, ContainerId, Detail, Error, FileName, METADATA_LIMIT, OpId, Pending,
    Record, Result, Version, VersionNum, invalid, normalize_tags,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub recycle_dir: PathBuf,
    pub password_hash: String,
    pub session_key: Blake3Hash,
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
        for path in [
            self.paths.system(),
            self.paths.containers(),
            self.paths.operations(),
            self.paths.active_stages(),
            self.paths.inflight(),
            self.paths.incoming(),
            self.paths.recycled(),
        ] {
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

    pub fn stage(&self, id: &ContainerId, op: &OpId) -> Result<StageGuard> {
        let path = self.paths.staging(id, op);
        let marker = self.paths.stage_marker(op);
        let guard = StageGuard {
            path,
            marker,
            status: StageStatus::Uncommitted,
        };
        atomic_json(&guard.marker, id)?;
        mkdir(parent(&guard.path)?)?;
        fs::create_dir(&guard.path)?;
        sync_parent(&guard.path)?;
        Ok(guard)
    }

    pub fn discard_index(&self) -> Result<()> {
        for suffix in ["wal", "shm"] {
            remove_file(&self.paths.index_sidecar(suffix))?;
        }
        remove_file(&self.paths.index())
    }

    pub fn start(&self, pending: &Pending) -> Result<()> {
        let path = self.paths.pending(pending.change.container_id());
        if path.try_exists()? {
            return Err(Error::Recovery);
        }
        atomic_json(&path, pending)
    }

    pub fn store_record(&self, record: &Record) -> Result<()> {
        let path = self.paths.operation(&record.receipt.op_id);
        mkdir(parent(&path)?)?;
        atomic_json(&path, record)
    }

    pub fn start_auth(&self, record: &Record) -> Result<()> {
        atomic_json(&self.paths.auth_pending(), record)
    }

    pub fn recover_auth(&self) -> Result<Option<Record>> {
        let path = self.paths.auth_pending();
        if path.try_exists()? {
            read_json(&path).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn end_auth(&self) -> Result<()> {
        remove_file(&self.paths.auth_pending())
    }

    pub fn end(&self, id: &ContainerId) -> Result<()> {
        remove_file(&self.paths.pending(id))
    }

    pub fn snapshot(path: &Path) -> Result<Detail> {
        let container: Container = read_json(&Layout::container_meta(path))?;
        normalize_tags(container.tags.clone())?;
        let mut versions = Vec::new();
        for dir in entries(&Layout::versions(path))? {
            let Some(number) = version_dir(&dir) else {
                continue;
            };
            let version: Version = read_json(&Layout::version_meta(&dir))?;
            let payload = version.payload_file.as_str();
            if version.number != number
                || (payload != "payload.bin" && payload != version.name_at_commit.payload_file())
            {
                return Err(invalid(format!(
                    "inconsistent version metadata in {}",
                    dir.display()
                )));
            }
            versions.push(version);
        }
        versions.sort_unstable_by_key(|v| v.number);
        if versions.last().map(|version| version.number) != Some(container.latest_version) {
            return Err(invalid(format!(
                "inconsistent latest version in {}",
                path.display()
            )));
        }
        Ok(Detail {
            container,
            versions,
        })
    }
}

pub struct StageGuard {
    pub path: PathBuf,
    marker: PathBuf,
    status: StageStatus,
}

enum StageStatus {
    Uncommitted,
    Journaled,
    Finished,
}

impl StageGuard {
    pub fn payload(&self, name: &FileName) -> PathBuf {
        Layout::payload_file(&self.path, name)
    }
    pub fn save_version(&self, version: &Version) -> Result<()> {
        atomic_json(&Layout::version_meta(&self.path), version)
    }
    pub fn journaled(&mut self) {
        self.status = StageStatus::Journaled;
    }
    pub fn finish(&mut self) -> Result<()> {
        remove_file(&self.marker)?;
        self.status = StageStatus::Finished;
        Ok(())
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if matches!(self.status, StageStatus::Uncommitted)
            && let Err(error) = clean_stage(&self.path, &self.marker)
        {
            tracing::error!(?error, "staging cleanup failed");
        }
    }
}

pub fn clean_stage(path: &Path, marker: &Path) -> Result<()> {
    remove_dir(path)?;
    if let Some(versions) = path.parent() {
        remove_empty(versions)?;
        if let Some(container) = versions.parent() {
            remove_empty(container)?;
        }
    }
    remove_file(marker)
}

fn remove_empty(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => sync_parent(path),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err.into()),
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

fn small(path: &Path) -> Result<Vec<u8>> {
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

fn atomic_bytes(path: &Path, value: &[u8]) -> Result<()> {
    if value.len() > METADATA_LIMIT {
        return Err(invalid("metadata exceeds 1 MiB"));
    }
    let temp = Layout::temporary(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(value)?;
    file.sync_all()?;
    rename(&temp, path)
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
