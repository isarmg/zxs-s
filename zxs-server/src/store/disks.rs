use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::ErrorKind,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::domain::{DiskId, DiskName, Error, Result, invalid};

use super::{Config, Store, fs_ops::small};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Data,
    Recycle,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredDisk {
    pub name: DiskName,
    pub disk_id: DiskId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    kind: DiskKind,
    disk_id: DiskId,
}

impl Store {
    pub fn bind_online(&mut self, config: &Config) -> Result<()> {
        let mut ids = HashSet::new();
        self.paths.data_disks = self.online_group(DiskKind::Data, &config.data_disks, &mut ids)?;
        self.paths.recycle_disks =
            self.online_group(DiskKind::Recycle, &config.recycle_disks, &mut ids)?;
        Ok(())
    }

    fn online_group(
        &self,
        kind: DiskKind,
        disks: &[RegisteredDisk],
        ids: &mut HashSet<DiskId>,
    ) -> Result<Vec<RegisteredDisk>> {
        let mut online = Vec::new();
        let mut names = BTreeSet::new();
        for disk in disks {
            if !names.insert(disk.name.key()) || !ids.insert(disk.disk_id.clone()) {
                return Err(invalid(format!("duplicate disk {}", disk.name)));
            }
            match self.check_disk(kind, disk) {
                Ok(()) => online.push(disk.clone()),
                Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                    tracing::warn!(?kind, disk = %disk.name, "disk is offline");
                }
                Err(error) => {
                    return Err(Error::Config(format!(
                        "{kind:?} disk {}: {error}",
                        disk.name
                    )));
                }
            }
        }
        Ok(online)
    }

    fn check_disk(&self, kind: DiskKind, disk: &RegisteredDisk) -> Result<()> {
        let physical = self.paths.disk_root(kind, &disk.name);
        let marker = self.paths.disk_marker(kind, &disk.name);
        if !fs::symlink_metadata(&physical)?.is_dir() {
            return Err(invalid("expected a physical disk directory"));
        }
        let found = read_marker(&marker)?;
        if found.disk_id.as_str() != disk.disk_id.as_str() || found.kind != kind {
            return Err(invalid("disk identity does not match its registration"));
        }
        Ok(())
    }

    pub fn extend_registrations(&self, config: &mut Config) -> Result<bool> {
        let data = self.register_disks(DiskKind::Data, &config.data_disks)?;
        let recycle = self.register_disks(DiskKind::Recycle, &config.recycle_disks)?;
        if data.is_empty() || recycle.is_empty() {
            return Err(invalid(
                "prepare at least one data disk and one recycle disk with zxs-prepare",
            ));
        }
        if data.len() == config.data_disks.len() && recycle.len() == config.recycle_disks.len() {
            return Ok(false);
        }
        let mut ids = HashSet::new();
        for disk in data.iter().chain(&recycle) {
            if !ids.insert(&disk.disk_id) {
                return Err(invalid(format!("duplicate disk {}", disk.name)));
            }
        }
        config.data_disks = data;
        config.recycle_disks = recycle;
        Ok(true)
    }

    pub fn register_disks(
        &self,
        kind: DiskKind,
        registered: &[RegisteredDisk],
    ) -> Result<Vec<RegisteredDisk>> {
        let mut disks = registered.to_vec();
        let children = match fs::read_dir(self.paths.root(kind)) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(disks),
            result => result?,
        };
        for child in children {
            let child = child?;
            let path = child.path();
            if path.file_name().is_some_and(|name| name == ".zxs") || !child.file_type()?.is_dir() {
                continue;
            }
            let raw_name = path.file_name().and_then(|name| name.to_str());
            if registered
                .iter()
                .any(|disk| Some(disk.name.as_str()) == raw_name)
            {
                continue;
            }
            let marker = path.join(".zxs/disk.toml");
            let marker = match read_marker(&marker) {
                Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => continue,
                result => result?,
            };
            let name =
                DiskName::parse(raw_name.ok_or_else(|| invalid("disk name must be UTF-8"))?)?;
            if disks.iter().any(|disk| disk.name.key() == name.key()) {
                return Err(invalid(format!("conflicting disk name {name}")));
            }
            if self.paths.disk_root(kind, &name) != path || marker.kind != kind {
                return Err(invalid(format!(
                    "invalid disk name or purpose for {}",
                    path.display()
                )));
            }
            disks.push(RegisteredDisk {
                disk_id: marker.disk_id,
                name,
            });
        }
        disks.sort_by_key(|disk| disk.name.key());
        Ok(disks)
    }

    pub fn recycle_targets(&self) -> Result<RecycleTargets> {
        let mut targets = Vec::new();
        for disk in &self.paths.recycle_disks {
            let path = self.paths.disk_root(DiskKind::Recycle, &disk.name);
            let stats = rustix::fs::statvfs(&path).map_err(std::io::Error::from)?;
            targets.push(Capacity {
                name: disk.name.clone(),
                bytes: stats.f_bavail.saturating_mul(stats.f_frsize),
            });
        }
        Ok(RecycleTargets(targets))
    }
}

struct Capacity {
    name: DiskName,
    bytes: u64,
}

pub struct RecycleTargets(Vec<Capacity>);

impl RecycleTargets {
    pub fn select(&mut self, bytes: u64) -> Result<DiskName> {
        if self.0.is_empty() {
            return Err(Error::RecycleOffline);
        }
        let best = self
            .0
            .iter_mut()
            .filter(|disk| disk.bytes >= bytes)
            .max_by_key(|disk| disk.bytes)
            .ok_or(Error::RecycleFull)?;
        best.bytes -= bytes;
        Ok(best.name.clone())
    }
}

fn read_marker(path: &Path) -> Result<Marker> {
    let bytes = small(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| Error::Config(error.to_string()))?;
    toml::from_str(text).map_err(|error| Error::Config(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use crate::test_support::{TestDir, prepare_disk};

    use super::{Config, Result, fs};

    #[test]
    fn missing_recycle_root_does_not_block_registration() -> Result<()> {
        let root = TestDir::create()?;
        let store = root.store()?;
        let mut config = Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            recycle_dir: store.paths.recycle.clone(),
            password_hash: String::new(),
            data_disks: store.paths.data_disks.clone(),
            recycle_disks: store.paths.recycle_disks.clone(),
        };
        fs::rename(&config.recycle_dir, root.0.join("offline"))?;
        prepare_disk(&store.paths.data.join("disk2"), "data", 3)?;

        assert!(store.extend_registrations(&mut config)?);
        assert_eq!(config.data_disks[1].name.as_str(), "disk2");
        assert_eq!(config.recycle_disks, store.paths.recycle_disks);
        Ok(())
    }
}
