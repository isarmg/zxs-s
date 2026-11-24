use std::path::{Path, PathBuf};

use crate::domain::{ContainerId, FileName, OpId, RecycleId, Version, VersionNum};

#[derive(Clone)]
pub struct Layout {
    pub data: PathBuf,
    pub recycle: PathBuf,
}

impl Layout {
    pub fn system(&self) -> PathBuf {
        self.data.join(".system")
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
    pub fn auth_pending(&self) -> PathBuf {
        self.system().join("auth-pending")
    }
    pub fn inflight(&self) -> PathBuf {
        self.system().join("inflight")
    }
    pub fn pending(&self, id: &ContainerId) -> PathBuf {
        self.inflight().join(id.as_str())
    }
    pub fn active_stages(&self) -> PathBuf {
        self.system().join("staging")
    }
    pub fn stage_marker(&self, op: &OpId) -> PathBuf {
        self.active_stages().join(op.as_str())
    }
    pub fn operations(&self) -> PathBuf {
        self.system().join("operations")
    }
    pub fn operation(&self, op: &OpId) -> PathBuf {
        shard(&self.operations(), op.as_str()).with_extension("json")
    }
    pub fn containers(&self) -> PathBuf {
        self.data.join("containers")
    }
    pub fn container(&self, id: &ContainerId) -> PathBuf {
        shard(&self.containers(), id.as_str())
    }
    pub fn staging(&self, id: &ContainerId, op: &OpId) -> PathBuf {
        Self::versions(&self.container(id)).join(format!(".staging-{op}"))
    }
    pub fn incoming(&self) -> PathBuf {
        self.recycle.join("incoming")
    }
    pub fn incoming_entry(&self, id: &RecycleId) -> PathBuf {
        self.incoming().join(id.as_str())
    }
    pub fn recycled(&self) -> PathBuf {
        self.recycle.join("containers")
    }
    pub fn recycled_entry(&self, id: &RecycleId) -> PathBuf {
        shard(&self.recycled(), id.as_str())
    }
    pub fn recycle_meta(entry: &Path) -> PathBuf {
        entry.join("recycle.json")
    }
    pub fn recycled_container(entry: &Path) -> PathBuf {
        entry.join("container")
    }
    pub fn container_meta(container: &Path) -> PathBuf {
        container.join("container.json")
    }
    pub fn versions(container: &Path) -> PathBuf {
        container.join("versions")
    }
    pub fn version(container: &Path, v: VersionNum) -> PathBuf {
        Self::versions(container).join(format!("v{:06}", v.get()))
    }
    pub fn version_meta(dir: &Path) -> PathBuf {
        dir.join("version.json")
    }
    pub fn payload_file(dir: &Path, file: &FileName) -> PathBuf {
        dir.join(file.as_str())
    }
    pub fn payload(&self, id: &ContainerId, version: &Version) -> PathBuf {
        Self::payload_file(
            &Self::version(&self.container(id), version.number),
            &version.payload_file,
        )
    }
    pub fn temporary(path: &Path) -> PathBuf {
        path.with_extension("tmp")
    }
    pub fn transfer_dir(path: &Path) -> PathBuf {
        path.with_extension("copying")
    }
    pub fn child(path: &Path, name: &std::ffi::OsStr) -> PathBuf {
        path.join(name)
    }
}

fn shard(path: &Path, id: &str) -> PathBuf {
    path.join(&id[..2]).join(id)
}
