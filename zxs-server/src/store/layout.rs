use std::path::{Path, PathBuf};

use crate::domain::{
    Container, DiskName, Error, FileName, FolderPath, RecycleId, RecycleInfo, Recycled, Result,
    TxId, Version, VersionNum, invalid,
};

use super::{DiskKind, RegisteredDisk};

#[derive(Clone)]
pub struct Layout {
    pub data: PathBuf,
    pub recycle: PathBuf,
    pub data_disks: Vec<RegisteredDisk>,
    pub recycle_disks: Vec<RegisteredDisk>,
}

impl Layout {
    pub fn new(data: PathBuf, recycle: PathBuf) -> Self {
        Self {
            data,
            recycle,
            data_disks: Vec::new(),
            recycle_disks: Vec::new(),
        }
    }
    pub fn disk_root(&self, kind: DiskKind, name: &DiskName) -> PathBuf {
        self.root(kind).join(name.as_str())
    }
    pub fn root(&self, kind: DiskKind) -> &Path {
        match kind {
            DiskKind::Data => &self.data,
            DiskKind::Recycle => &self.recycle,
        }
    }
    pub fn disk_marker(&self, kind: DiskKind, name: &DiskName) -> PathBuf {
        self.disk_root(kind, name).join(".zxs/disk.toml")
    }
    pub fn data_disk(&self, folder: &FolderPath) -> Result<DiskName> {
        let name = folder.disk()?;
        if self.data_disks.iter().any(|disk| disk.name == name) {
            return Ok(name);
        }
        if let Some(disk) = self
            .data_disks
            .iter()
            .find(|disk| disk.name.key() == name.key())
        {
            return Err(Error::Conflict(format!(
                "disk is registered as {}",
                disk.name
            )));
        }
        Err(invalid(format!("unknown data disk: {name}")))
    }
    pub fn system(&self) -> PathBuf {
        self.data.join(".zxs")
    }
    pub fn config(&self) -> PathBuf {
        self.system().join("config.toml")
    }
    pub fn lock(&self) -> PathBuf {
        self.system().join(".lock")
    }
    pub fn index_sidecar(&self, suffix: &str) -> PathBuf {
        self.system().join(format!("index.sqlite-{suffix}"))
    }
    pub fn index(&self) -> PathBuf {
        self.system().join("index.sqlite")
    }
    pub fn inflight(&self) -> PathBuf {
        self.system().join("inflight")
    }
    pub fn pending(&self, op: &TxId) -> PathBuf {
        self.inflight().join(op.as_str()).with_extension("json")
    }
    pub fn spools(&self) -> Vec<PathBuf> {
        self.data_disks
            .iter()
            .flat_map(|disk| {
                [
                    self.staging_root(&disk.name),
                    self.live_trash_root(&disk.name),
                ]
            })
            .chain(
                self.recycle_disks
                    .iter()
                    .flat_map(|disk| [self.incoming(&disk.name), self.purging(&disk.name)]),
            )
            .collect()
    }
    pub fn require_recycle_disk(&self, name: &DiskName) -> Result<()> {
        if self.recycle_disks.iter().any(|disk| disk.name == *name) {
            Ok(())
        } else {
            Err(invalid(format!("recycle disk unavailable: {name}")))
        }
    }
    pub fn staging_root(&self, disk: &DiskName) -> PathBuf {
        self.disk_root(DiskKind::Data, disk).join(".zxs/staging")
    }
    pub fn staging(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.staging_root(disk).join(tx.as_str())
    }
    pub fn folder(&self, folder: &FolderPath) -> PathBuf {
        self.data.join(relative_folder(folder))
    }
    pub fn folder_at(&self, path: &Path) -> Result<FolderPath> {
        let relative = path
            .strip_prefix(&self.data)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| invalid(format!("invalid directory path {}", path.display())))?;
        let physical = format!("/{relative}");
        let folder = FolderPath::parse(&physical)?;
        if folder.as_str() != physical {
            return Err(invalid(format!(
                "storage name is not NFC: {}",
                path.display()
            )));
        }
        Ok(folder)
    }
    pub fn container_location(&self, path: &Path) -> Result<(FolderPath, FileName)> {
        let location = self.folder_at(path)?;
        let folder = location
            .parent()
            .ok_or_else(|| invalid("storage root is not a container"))?;
        let (_, name) = location
            .as_str()
            .rsplit_once('/')
            .ok_or_else(|| invalid("container has no file name"))?;
        Ok((folder, FileName::parse(name)?))
    }
    pub fn container_at(&self, folder: &FolderPath, name: &FileName) -> PathBuf {
        self.data.join(relative_folder(folder)).join(name.as_str())
    }
    pub fn container(&self, container: &Container) -> PathBuf {
        self.container_at(&container.folder, &container.name)
    }
    pub fn incoming(&self, disk: &DiskName) -> PathBuf {
        self.disk_root(DiskKind::Recycle, disk)
            .join(".zxs/incoming")
    }
    pub fn incoming_operation(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.incoming(disk).join(tx.as_str())
    }
    pub fn incoming_entry(&self, disk: &DiskName, tx: &TxId, id: &RecycleId) -> PathBuf {
        self.incoming_operation(disk, tx).join(id.as_str())
    }
    pub fn staged_tree(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.staging(disk, tx).join("tree")
    }
    pub fn staged_version(stage: &Path) -> PathBuf {
        stage.join("version")
    }
    pub fn prepared_metadata(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.staging(disk, tx).join("container.json")
    }
    pub fn live_trash_root(&self, disk: &DiskName) -> PathBuf {
        self.disk_root(DiskKind::Data, disk).join(".zxs/purging")
    }
    pub fn live_trash(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.live_trash_root(disk).join(tx.as_str())
    }
    pub fn tree_archives(&self, tx: &TxId) -> PathBuf {
        self.inflight().join(tx.as_str()).with_extension("items")
    }
    pub fn tree_archive_plan(&self, tx: &TxId) -> PathBuf {
        self.tree_archives(tx).join("plan.json")
    }
    pub fn purging(&self, disk: &DiskName) -> PathBuf {
        self.disk_root(DiskKind::Recycle, disk).join(".zxs/purging")
    }
    pub fn trash(&self, disk: &DiskName, tx: &TxId) -> PathBuf {
        self.purging(disk).join(tx.as_str())
    }
    pub fn trash_entry(&self, tx: &TxId, entry: &Recycled) -> PathBuf {
        self.trash(&entry.recycle_disk, tx)
            .join(entry.recycle.recycle_id.as_str())
    }
    pub fn recycled_entry(&self, entry: &Recycled) -> PathBuf {
        self.recycle_batch(&entry.recycle_disk, &entry.recycle)
    }
    pub fn recycle_batch(&self, disk: &DiskName, recycle: &RecycleInfo) -> PathBuf {
        let date = recycle_date(recycle.recycled_at);
        self.disk_root(DiskKind::Recycle, disk)
            .join(format!("{date}_{}", recycle.recycle_id))
    }
    pub fn recycled_container(entry: &Path, name: &FileName) -> PathBuf {
        entry.join(name.as_str())
    }
    pub fn metadata(directory: &Path) -> PathBuf {
        directory.join(".zxs")
    }
    pub fn version(container: &Path, v: VersionNum) -> PathBuf {
        container.join(format!("v{:06}", v.get()))
    }
    pub fn payload_file(dir: &Path, file: &FileName) -> PathBuf {
        dir.join(file.as_str())
    }
    pub fn payload(container: &Path, version: &Version) -> PathBuf {
        Self::payload_file(
            &Self::version(container, version.number),
            &version.name_at_commit,
        )
    }
    pub fn temporary(path: &Path) -> PathBuf {
        path.with_extension("tmp")
    }
}

fn relative_folder(folder: &FolderPath) -> &str {
    folder.as_str().trim_start_matches('/')
}

fn recycle_date(unix_seconds: u64) -> String {
    let days = unix_seconds / 86_400;
    let mut year = 1970 + days / 146_097 * 400;
    let mut day = days % 146_097;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    loop {
        let length = 365 + u64::from(leap(year));
        if day < length {
            break;
        }
        year += 1;
        day -= length;
    }
    let mut month = 1;
    for length in [
        31,
        28 + u64::from(leap(year)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ] {
        if day < length {
            break;
        }
        month += 1;
        day -= length;
    }
    format!("{year:04}-{month:02}-{:02}", day + 1)
}
