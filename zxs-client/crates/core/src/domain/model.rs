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
    pub latest_blake3: Blake3Hash,
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

impl Version {
    #[must_use]
    pub fn preview_name<'a>(&'a self, container: &'a Container) -> &'a FileName {
        let current = (self.version == container.latest_version).then_some(&container.name);
        current.unwrap_or(&self.name_at_commit)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub container: Container,
    pub versions: Vec<Version>,
}

impl Detail {
    /// Selects an explicit version or the current latest version.
    /// # Errors
    /// Returns an error if the version does not exist in this history.
    pub fn select_version(self, number: Option<VersionNum>) -> super::Result<(Container, Version)> {
        let number = number.unwrap_or(self.container.latest_version);
        let version = self
            .versions
            .into_iter()
            .find(|version| version.version == number)
            .ok_or_else(|| super::Error::Remote {
                status: 404,
                message: format!("version {number} not found"),
            })?;
        Ok((self.container, version))
    }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restore {
    pub folder: FolderPath,
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
    pub container: ArchivedContainer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedContainer {
    pub container_id: ContainerId,
    pub name: FileName,
    pub tags: Vec<String>,
    pub latest_version: VersionNum,
    pub latest_blake3: Blake3Hash,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub folder: Option<FolderPath>,
    #[serde(default)]
    pub recursive: bool,
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
