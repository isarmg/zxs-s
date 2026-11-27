mod publication;
#[cfg(test)]
mod tests;

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::{
    Result, build, invalid,
    paths::{self, Entry, EntryKind, SYSTEM},
};

use publication::Pending;

const MARKER: &str = "disk.toml";
const WORK: &str = "prepare";
const PENDING: &str = "pending.json";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Data,
    Recycle,
}

impl DiskKind {
    fn label(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Recycle => "recycle",
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Empty,
    Data,
    Pending,
    Prepared,
}

#[derive(Serialize)]
pub struct Scan {
    root: PathBuf,
    mount_name: Option<String>,
    status: Status,
    entries: Vec<Listed>,
}

#[derive(Serialize)]
struct Listed {
    path: String,
    name: String,
    kind: EntryKind,
    bytes: u64,
}

#[derive(Serialize)]
pub struct Report {
    root: PathBuf,
    kind: DiskKind,
    disk_id: String,
    containers: usize,
    originals: Option<PathBuf>,
}

pub fn scan(path: &Path, within: &str) -> Result<Scan> {
    let disk = Disk::read(path)?;
    let (status, entries) = if paths::exists(&disk.pending())? {
        (Status::Pending, Vec::new())
    } else if paths::exists(&disk.marker())? {
        (Status::Prepared, Vec::new())
    } else {
        disk.check_system()?;
        browse(&disk.root, within)?
    };
    Ok(Scan {
        mount_name: default_mount_name(&disk.root).map(str::to_owned),
        root: disk.root,
        status,
        entries,
    })
}

fn browse(root: &Path, within: &str) -> Result<(Status, Vec<Listed>)> {
    let directory = paths::within(root, within)?;
    let entries = paths::entries(&directory, within.is_empty())?;
    let status = if within.is_empty() && entries.is_empty() {
        Status::Empty
    } else {
        Status::Data
    };
    let entries = entries
        .into_iter()
        .map(|entry| Listed {
            path: paths::child(within, &entry.name),
            name: entry.name,
            kind: entry.kind,
            bytes: entry.bytes,
        })
        .collect();
    Ok((status, entries))
}

pub fn init(path: &Path, kind: DiskKind) -> Result<Report> {
    let disk = Disk::read(path)?;
    if !paths::entries(&disk.root, true)?.is_empty() {
        return Err(invalid("初始化空盘要求所选目录为空"));
    }
    let _lock = disk.lock()?;
    disk.require_fresh()?;
    if !paths::entries(&disk.root, true)?.is_empty() {
        return Err(invalid("所选目录已有数据"));
    }
    clean_stage(&disk.stage())?;
    disk.commit(Pending {
        kind,
        disk_id: build::identifier(None)?,
        originals: Vec::new(),
        published: Vec::new(),
        containers: 0,
    })
}

pub fn prepare(path: &Path, mount_name: Option<&str>, bundle: &[String]) -> Result<Report> {
    let disk = Disk::read(path)?;
    let mount = mount_name
        .or_else(|| default_mount_name(&disk.root))
        .ok_or_else(|| invalid("请指定服务端的一级挂载目录名"))?;
    if paths::name(mount)? != mount {
        return Err(invalid(
            "服务端挂载名须采用 NFC，或用 --mount-name 指定实际挂载名",
        ));
    }
    let _lock = disk.lock()?;
    disk.require_fresh()?;
    let stage = disk.stage();
    clean_stage(&stage)?;
    fs::create_dir(&stage)?;
    let tree = match build::stage(&disk.root, &stage, mount, bundle).and_then(|tree| {
        check_names(&paths::entries(&disk.root, true)?, &tree.originals)?;
        Ok(tree)
    }) {
        Ok(tree) => tree,
        Err(error) => {
            if let Err(cleanup) = clean_stage(&stage) {
                eprintln!("准备暂存未能清理 {} · {cleanup}", stage.display());
            }
            return Err(error);
        }
    };
    disk.commit(Pending {
        kind: DiskKind::Data,
        disk_id: build::identifier(None)?,
        originals: tree.originals,
        published: tree.published,
        containers: tree.containers,
    })
}

fn default_mount_name(root: &Path) -> Option<&str> {
    root.file_name()?.to_str()
}

pub fn resume(path: &Path) -> Result<Report> {
    let disk = Disk::read(path)?;
    if !paths::exists(&disk.pending())? {
        return Err(invalid("此盘没有待完成的准备"));
    }
    let _lock = disk.lock()?;
    let pending = serde_json::from_reader(File::open(disk.pending())?)?;
    disk.finish(pending)
}

struct Disk {
    root: PathBuf,
    system: PathBuf,
    work: PathBuf,
}

impl Disk {
    fn read(path: &Path) -> Result<Self> {
        let root = paths::root(path)?;
        let system = root.join(SYSTEM);
        let work = system.join(WORK);
        for directory in [&system, &work] {
            if paths::exists(directory)?
                && paths::kind(&fs::symlink_metadata(directory)?) != EntryKind::Directory
            {
                return Err(invalid(format!("这里应是普通目录 {}", directory.display())));
            }
        }
        Ok(Self { root, system, work })
    }

    fn pending(&self) -> PathBuf {
        self.work.join(PENDING)
    }

    fn marker(&self) -> PathBuf {
        self.system.join(MARKER)
    }

    fn stage(&self) -> PathBuf {
        self.work.join("tree")
    }

    fn check_system(&self) -> Result<()> {
        if paths::exists(&self.system)?
            && paths::entries(&self.system, false)?
                .iter()
                .any(|entry| entry.name != WORK)
        {
            return Err(invalid(format!(
                "已有其他 .zxs 数据 {}",
                self.system.display()
            )));
        }
        Ok(())
    }

    fn lock(&self) -> Result<File> {
        if !paths::exists(&self.pending())? {
            if paths::exists(&self.marker())? {
                return Err(invalid("磁盘已准备，原盘身份已保留"));
            }
            self.check_system()?;
        }
        paths::mkdir(&self.system)?;
        paths::mkdir(&self.work)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.work.join(".lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => Err(invalid("另一个进程正在准备此盘")),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn require_fresh(&self) -> Result<()> {
        if paths::exists(&self.pending())? {
            Err(invalid("此盘已有准备记录，请使用 resume"))
        } else if paths::exists(&self.marker())? {
            Err(invalid("此盘已准备"))
        } else {
            self.check_system()
        }
    }
}

fn check_names(entries: &[Entry], expected: &[String]) -> Result<()> {
    if entries.iter().map(|entry| &entry.name).eq(expected.iter()) {
        Ok(())
    } else {
        Err(invalid("准备期间磁盘根目录发生变动"))
    }
}

fn clean_stage(stage: &Path) -> Result<()> {
    if paths::exists(stage)? {
        if paths::kind(&fs::symlink_metadata(stage)?) != EntryKind::Directory {
            return Err(invalid(format!("无效的准备暂存 {}", stage.display())));
        }
        #[cfg(windows)]
        remove_readonly(stage)?;
        fs::remove_dir_all(stage)?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "clears only the Windows read-only attribute on staged files"
)]
fn remove_readonly(path: &Path) -> Result<()> {
    for entry in paths::entries(path, false)? {
        if entry.kind == EntryKind::Directory {
            remove_readonly(&entry.path)?;
        } else if entry.kind == EntryKind::File {
            let mut permissions = fs::metadata(&entry.path)?.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                fs::set_permissions(entry.path, permissions)?;
            }
        }
    }
    Ok(())
}
