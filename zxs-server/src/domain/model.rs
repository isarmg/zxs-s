use serde::{Deserialize, Serialize};

use super::{Blake3Hash, ContainerId, FileName, FolderPath, OpId, RecycleId, Result, VersionNum};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    #[serde(rename = "container_id")]
    pub id: ContainerId,
    pub folder: FolderPath,
    pub name: FileName,
    pub tags: Vec<String>,
    pub latest_version: VersionNum,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    #[serde(rename = "version")]
    pub number: VersionNum,
    pub op_id: OpId,
    pub created_at: u64,
    pub name_at_commit: FileName,
    pub size: u64,
    pub blake3: Blake3Hash,
    pub payload_file: FileName,
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
    pub op_id: OpId,
    pub recycled_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recycled {
    pub recycle: RecycleInfo,
    pub container: Container,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub folder: Option<FolderPath>,
    pub tag: Option<String>,
    pub q: Option<String>,
    pub cursor: Option<ContainerId>,
    pub limit: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleFilter {
    pub cursor: Option<RecycleId>,
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct FolderSummary {
    pub folder: FolderPath,
    pub count: u64,
}

#[derive(Debug, Serialize)]
pub struct ContainerPage {
    pub containers: Vec<Container>,
    pub folders: Vec<FolderSummary>,
    pub next_cursor: Option<ContainerId>,
}

#[derive(Debug, Serialize)]
pub struct RecyclePage {
    pub entries: Vec<Recycled>,
    pub next_cursor: Option<RecycleId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub container: Container,
    pub version: Version,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub op_id: OpId,
    pub status: u16,
    pub result: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub receipt: Receipt,
    pub fingerprint: Blake3Hash,
    pub session: Option<SessionEffect>,
}

impl Record {
    pub fn new(
        op_id: OpId,
        fingerprint: Blake3Hash,
        status: u16,
        result: impl Serialize,
    ) -> Result<Self> {
        Ok(Self {
            receipt: Receipt {
                op_id,
                status,
                result: serde_json::to_value(result)?,
            },
            fingerprint,
            session: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum SessionEffect {
    Login {
        token_blake3: Blake3Hash,
        expires_at: u64,
        generation: Blake3Hash,
    },
    Logout {
        token_blake3: Blake3Hash,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum Change {
    Publish {
        commit: Box<Commit>,
    },
    Patch {
        container: Container,
    },
    Recycle {
        entry: Recycled,
    },
    Restore {
        container: Container,
        recycle_id: RecycleId,
    },
    Prune {
        container_id: ContainerId,
        versions: Vec<VersionNum>,
    },
}

impl Change {
    pub fn container_id(&self) -> &ContainerId {
        match self {
            Self::Publish { commit } => &commit.container.id,
            Self::Patch { container } | Self::Restore { container, .. } => &container.id,
            Self::Recycle { entry } => &entry.container.id,
            Self::Prune { container_id, .. } => container_id,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub change: Change,
    pub record: Option<Record>,
}
