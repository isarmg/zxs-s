use serde::{Deserialize, Serialize};

use super::{
    Blake3Hash, ContainerCursor, ContainerId, DiskName, FileName, FolderPath, RecycleCursor,
    RecycleId, Result, SessionToken, TxId, VersionNum, cursor,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    #[serde(rename = "container_id")]
    pub id: ContainerId,
    pub folder: FolderPath,
    pub name: FileName,
    pub tags: Vec<String>,
    pub latest_version: VersionNum,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_blake3: Option<Blake3Hash>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerMeta {
    #[serde(rename = "container_id")]
    pub id: ContainerId,
    pub tags: Vec<String>,
    pub latest_version: VersionNum,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Container {
    pub fn at(folder: FolderPath, name: FileName, meta: ContainerMeta) -> Self {
        Self {
            id: meta.id,
            folder,
            name,
            tags: meta.tags,
            latest_version: meta.latest_version,
            latest_blake3: None,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
        }
    }

    pub fn metadata(&self) -> ContainerMeta {
        ContainerMeta {
            id: self.id.clone(),
            tags: self.tags.clone(),
            latest_version: self.latest_version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    #[serde(rename = "version")]
    pub number: VersionNum,
    pub created_at: u64,
    pub name_at_commit: FileName,
    pub size: u64,
    pub blake3: Blake3Hash,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub container: Container,
    pub versions: Vec<Version>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewContainer {
    pub folder: FolderPath,
    pub name: FileName,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewVersion {
    pub expected_version: VersionNum,
    pub revert_to_version: Option<VersionNum>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub folder: Option<FolderPath>,
    pub name: Option<FileName>,
    pub tags: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restore {
    pub folder: Option<FolderPath>,
    pub name: Option<FileName>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleInfo {
    pub recycle_id: RecycleId,
    pub recycled_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recycled {
    pub recycle: RecycleInfo,
    pub container: Container,
    pub recycle_disk: DiskName,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub folder: Option<FolderPath>,
    pub tag: Option<String>,
    pub q: Option<String>,
    #[serde(default, deserialize_with = "cursor::decode")]
    pub cursor: Option<ContainerCursor>,
    pub limit: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleFilter {
    #[serde(default, deserialize_with = "cursor::decode")]
    pub cursor: Option<RecycleCursor>,
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ContainerPage {
    pub containers: Vec<Container>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Folder {
    pub folder: FolderPath,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderMove {
    pub from: FolderPath,
    pub to: FolderPath,
}

#[derive(Debug, Serialize)]
pub struct FolderPage {
    pub folders: Vec<Folder>,
}

#[derive(Debug, Serialize)]
pub struct RecyclePage {
    pub entries: Vec<Recycled>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub container: Container,
    pub version: Version,
}

#[derive(Serialize)]
pub struct Session {
    pub token: SessionToken,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum Change {
    Publish {
        commit: Box<Commit>,
    },
    Patch {
        previous: Container,
        container: Container,
        staged: bool,
    },
    Recycle {
        entry: Recycled,
        staged: bool,
    },
    Restore {
        container: Container,
        entry: Recycled,
        staged: bool,
    },
    Prune {
        container: Container,
        versions: Vec<VersionNum>,
    },
    CreateFolder {
        folder: FolderPath,
    },
    MoveFolder {
        from: FolderPath,
        to: FolderPath,
        staged: bool,
    },
    DeleteFolder {
        folder: FolderPath,
    },
    RecycleTree {
        folder: FolderPath,
    },
    Purge {
        entry: Option<Box<Recycled>>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub tx_id: TxId,
    pub change: Change,
}

pub fn tree_recycle(
    op: &TxId,
    at: u64,
    container: Container,
    recycle_disk: DiskName,
) -> Result<Recycled> {
    let hash = blake3::hash(format!("{op}{}", container.id).as_bytes()).to_hex();
    let recycle = RecycleInfo {
        recycle_id: RecycleId::parse(&hash.as_str()[..32])?,
        recycled_at: at,
    };
    Ok(Recycled {
        recycle,
        container,
        recycle_disk,
    })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecyclePlan {
    pub entry: Recycled,
    pub staged: bool,
}
