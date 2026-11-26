use serde::{Deserialize, Serialize};

use super::{
    Blake3Hash, ContainerId, DiskName, FileName, FolderPath, RecycleId, SessionToken, VersionNum,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub container_id: ContainerId,
    pub folder: FolderPath,
    pub name: FileName,
    pub tags: Vec<String>,
    pub latest_version: VersionNum,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_blake3: Option<Blake3Hash>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub version: VersionNum,
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
    pub recycle_disk: DiskName,
    pub container: Container,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub folder: Option<FolderPath>,
    pub tag: Option<String>,
    pub q: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleFilter {
    pub cursor: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPage {
    pub folders: Vec<Folder>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerPage {
    pub containers: Vec<Container>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecyclePage {
    pub entries: Vec<Recycled>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Purged {
    pub purged: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub container: Container,
    pub version: Version,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub token: SessionToken,
}
