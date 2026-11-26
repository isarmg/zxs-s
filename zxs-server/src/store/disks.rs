use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, OpenOptions},
    os::unix::fs::MetadataExt,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::domain::{DiskId, DiskName, Error, Result, invalid};

use super::{
    Config, Store,
    fs_ops::{atomic_bytes, entries, mkdir, parent, small},
};

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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    kind: DiskKind,
    disk_id: DiskId,
}

impl Store {
    pub fn bind(&mut self, config: &Config) {
        self.paths.data_disks.clone_from(&config.data_disks);
        self.paths.recycle_disks.clone_from(&config.recycle_disks);
    }

    pub fn validate_disks(&mut self, verify: bool) -> Result<()> {
        let mut ids = HashSet::new();
        self.validate_group(DiskKind::Data, &self.paths.data_disks, &mut ids, !verify)?;
        if verify {
            let mut mounted = Vec::new();
            for disk in &self.paths.recycle_disks {
                let marker = self.paths.disk_marker(DiskKind::Recycle, &disk.name);
                match fs::symlink_metadata(marker) {
                    Ok(_) => mounted.push(disk.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tracing::warn!(disk = %disk.name, "recycle disk unavailable during verify");
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            self.paths.recycle_disks = mounted;
            if self.paths.recycle_disks.is_empty() {
                return Ok(());
            }
        }
        self.validate_group(
            DiskKind::Recycle,
            &self.paths.recycle_disks,
            &mut ids,
            !verify,
        )
    }

    fn validate_group(
        &self,
        kind: DiskKind,
        disks: &[RegisteredDisk],
        ids: &mut HashSet<DiskId>,
        writable: bool,
    ) -> Result<()> {
        if disks.is_empty() {
            return Err(invalid(
                "register at least one data disk and one recycle disk",
            ));
        }
        let mut names = BTreeSet::new();
        for disk in disks {
            if !names.insert(disk.name.key()) || !ids.insert(disk.disk_id.clone()) {
                return Err(invalid(format!("duplicate disk {}", disk.name)));
            }
            let result = self.check_disk(kind, disk, writable);
            result
                .map_err(|error| Error::Config(format!("{kind:?} disk {}: {error}", disk.name)))?;
        }
        Ok(())
    }

    fn check_disk(&self, kind: DiskKind, disk: &RegisteredDisk, writable: bool) -> Result<()> {
        let physical = self.paths.disk_root(kind, &disk.name);
        let marker = self.paths.disk_marker(kind, &disk.name);
        if !fs::symlink_metadata(&physical)?.is_dir()
            || !fs::symlink_metadata(parent(&marker)?)?.is_dir()
        {
            return Err(invalid("expected a physical disk directory"));
        }
        let found = read_marker(&marker)?;
        if found.disk_id.as_str() != disk.disk_id.as_str() || found.kind != kind {
            return Err(invalid("disk identity does not match its registration"));
        }
        if writable {
            OpenOptions::new().write(true).open(marker)?;
        }
        Ok(())
    }

    pub fn extend_registrations(&self, config: &mut Config) -> Result<bool> {
        let data = self.register_disks(DiskKind::Data, &config.data_disks)?;
        let recycle = self.register_disks(DiskKind::Recycle, &config.recycle_disks)?;
        if data.is_empty() || recycle.is_empty() {
            return Err(invalid(
                "register at least one data disk and one recycle disk",
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
        for path in entries(self.paths.root(kind))? {
            let raw_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("disk name must be UTF-8"))?;
            if raw_name == ".zxs" {
                continue;
            }
            let name = DiskName::parse(raw_name)?;
            if let Some(existing) = disks.iter().find(|disk| disk.name.key() == name.key()) {
                if existing.name.as_str() == raw_name {
                    continue;
                }
                return Err(invalid(format!("conflicting disk name {name}")));
            }
            if self.paths.disk_root(kind, &name) != path || !fs::symlink_metadata(&path)?.is_dir() {
                return Err(invalid(format!(
                    "invalid disk directory {}",
                    path.display()
                )));
            }
            disks.push(RegisteredDisk {
                disk_id: self.register_disk(kind, &name)?,
                name,
            });
        }
        disks.sort_by_key(|disk| disk.name.key());
        Ok(disks)
    }

    fn register_disk(&self, kind: DiskKind, name: &DiskName) -> Result<DiskId> {
        let path = self.paths.disk_marker(kind, name);
        let system = parent(&path)?;
        if system.try_exists()? && !fs::symlink_metadata(system)?.is_dir() {
            return Err(invalid(format!("invalid disk system directory for {name}")));
        }
        if path.try_exists()? {
            let marker = read_marker(&path)?;
            if marker.kind != kind {
                return Err(invalid(format!(
                    "disk {name} is marked for another purpose"
                )));
            }
            OpenOptions::new().write(true).open(&path)?;
            return Ok(marker.disk_id);
        }
        mkdir(system)?;
        let disk_id = random_id()?;
        let text = toml::to_string(&Marker {
            kind,
            disk_id: disk_id.clone(),
        })
        .map_err(|error| Error::Config(error.to_string()))?;
        atomic_bytes(&path, text.as_bytes())?;
        Ok(disk_id)
    }

    pub fn recycle_targets(&self, source: &DiskName) -> Result<RecycleTargets> {
        let device = fs::metadata(self.paths.disk_root(DiskKind::Data, source))?.dev();
        let mut targets = Vec::new();
        for disk in &self.paths.recycle_disks {
            let path = self.paths.disk_root(DiskKind::Recycle, &disk.name);
            let stats = rustix::fs::statvfs(&path).map_err(std::io::Error::from)?;
            targets.push(Capacity {
                name: disk.name.clone(),
                same_device: fs::metadata(path)?.dev() == device,
                bytes: stats.f_bavail.saturating_mul(stats.f_frsize),
            });
        }
        Ok(RecycleTargets(targets))
    }
}

struct Capacity {
    name: DiskName,
    same_device: bool,
    bytes: u64,
}

pub struct RecycleTargets(Vec<Capacity>);

impl RecycleTargets {
    pub fn select(&mut self, bytes: u64) -> Result<(DiskName, bool)> {
        let best = self
            .0
            .iter_mut()
            .filter(|disk| disk.same_device || disk.bytes >= bytes)
            .max_by_key(|disk| (disk.same_device, disk.bytes))
            .ok_or(Error::RecycleFull)?;
        if !best.same_device {
            best.bytes -= bytes;
        }
        Ok((best.name.clone(), !best.same_device))
    }
}

fn read_marker(path: &Path) -> Result<Marker> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("expected a regular disk marker"));
    }
    let bytes = small(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| Error::Config(error.to_string()))?;
    toml::from_str(text).map_err(|error| Error::Config(error.to_string()))
}

fn random_id() -> Result<DiskId> {
    use std::fmt::Write;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| Error::Config(error.to_string()))?;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(id, "{byte:02x}");
    }
    DiskId::parse(id)
}
