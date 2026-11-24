use serde::{Deserialize, Serialize};

use super::{
    Blake3Hash, ContainerId, FileName, FolderPath, OpId, RecycleId, SessionToken, VersionNum,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub container_id: ContainerId,
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
    pub version: VersionNum,
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

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub folder: Option<FolderPath>,
    pub tag: Option<String>,
    pub q: Option<String>,
    pub cursor: Option<ContainerId>,
    pub limit: Option<u16>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleFilter {
    pub cursor: Option<RecycleId>,
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderSummary {
    pub folder: FolderPath,
    pub count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerPage {
    pub containers: Vec<Container>,
    pub folders: Vec<FolderSummary>,
    pub next_cursor: Option<ContainerId>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub token: SessionToken,
    pub expires_at: u64,
}
