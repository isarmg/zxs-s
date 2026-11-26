use std::{collections::BTreeMap, time::SystemTime};

use serde::{Deserialize, Serialize};

use super::{Blake3Hash, Container, ContainerId, Error, FileName, Result, VersionNum, invalid};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocalPath(String);

impl LocalPath {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = super::types::normalized(value.into());
        if value == "." {
            return Ok(Self(value));
        }
        if value
            .split('/')
            .any(|segment| FileName::parse(segment).is_err())
        {
            return Err(invalid(format!("invalid local path: {value}")));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn key(&self) -> String {
        self.0.to_lowercase()
    }
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == "."
    }
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }
    #[must_use]
    pub fn below(&self, parent: &LocalPath) -> bool {
        self != parent
            && (parent.is_root()
                || self
                    .0
                    .strip_prefix(&parent.0)
                    .is_some_and(|tail| tail.starts_with('/')))
    }

    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        FileName::parse(name)?;
        if self.is_root() {
            Self::parse(name)
        } else {
            Self::parse(format!("{}/{name}", self.0))
        }
    }

    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            None
        } else {
            Some(Self(
                self.0
                    .rsplit_once('/')
                    .map_or(".", |(prefix, _)| prefix)
                    .into(),
            ))
        }
    }
}

impl TryFrom<String> for LocalPath {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<LocalPath> for String {
    fn from(value: LocalPath) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub size: u64,
    pub mtime_ns: i64,
}

impl Stamp {
    pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Result<Self> {
        let time = metadata.modified()?;
        let nanos = match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => nanoseconds(duration),
            Err(error) => -nanoseconds(error.duration()),
        };
        let mtime_ns = i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX });
        Ok(Self {
            size: metadata.len(),
            mtime_ns,
        })
    }
}

fn nanoseconds(duration: std::time::Duration) -> i128 {
    i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub path: LocalPath,
    pub kind: EntryKind,
    pub stamp: Option<Stamp>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "entries",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Snapshot {
    File(Stamp),
    Bundle(Vec<Member>),
}

pub type MemberHashes = BTreeMap<LocalPath, Blake3Hash>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ContentHashes {
    File(Blake3Hash),
    Bundle(MemberHashes),
}

#[derive(Default)]
pub struct CheckoutFile {
    pub old: Option<Blake3Hash>,
    pub new: Option<Blake3Hash>,
}

pub struct BundlePlan {
    pub files: BTreeMap<LocalPath, CheckoutFile>,
    pub members: BTreeMap<LocalPath, EntryKind>,
    pub remove_directories: Vec<LocalPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterruptedCheckout {
    pub path: LocalPath,
    pub container_id: ContainerId,
    pub version: VersionNum,
    pub endpoint: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<Blake3Hash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_against: Option<Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub version: VersionNum,
    pub blake3: Option<Blake3Hash>,
}

impl From<&Container> for Revision {
    fn from(container: &Container) -> Self {
        Self {
            version: container.latest_version,
            blake3: Some(container.latest_blake3.clone()),
        }
    }
}

pub fn revision_conflict(
    version: VersionNum,
    hash: Option<&Blake3Hash>,
    container: &Container,
) -> bool {
    version == container.latest_version
        && hash.is_some_and(|known| known != &container.latest_blake3)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub path: LocalPath,
    pub container_id: ContainerId,
    pub version: VersionNum,
    pub endpoint: String,
    pub snapshot: Snapshot,
    pub hashes: ContentHashes,
    pub remote_missing: bool,
    pub mirrored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_blake3: Option<Blake3Hash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<VersionNum>,
}

impl Track {
    pub fn baseline_hash(&self) -> Option<&Blake3Hash> {
        self.remote_blake3.as_ref().or(match &self.hashes {
            ContentHashes::File(hash) if self.restored_from.is_none() => Some(hash),
            _ => None,
        })
    }

    pub fn baseline(&self) -> Revision {
        Revision {
            version: self.version,
            blake3: self.baseline_hash().cloned(),
        }
    }

    pub fn check_revision(&self, container: &Container) -> Result<()> {
        if container.latest_version < self.version {
            return Err(Error::LocalConflict(format!(
                "远端版本已从 v{} 回退到 v{}，已保留本地内容",
                self.version, container.latest_version
            )));
        }
        if revision_conflict(self.version, self.baseline_hash(), container) {
            return Err(Error::LocalConflict(format!(
                "远端 v{} 的内容与已追踪的版本不一致，已保留本地内容",
                self.version
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Untracked,
    Clean,
    Modified,
    Missing,
    Interrupted,
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub path: LocalPath,
    pub container_id: Option<ContainerId>,
    pub version: Option<VersionNum>,
    pub bundle: bool,
    pub state: FileState,
    #[serde(skip)]
    pub(crate) file_digest: Option<(Stamp, Blake3Hash)>,
    pub remote_missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<VersionNum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use crate::domain::Result;

    use super::{FileName, LocalPath};

    #[test]
    fn portable_paths_cover_windows_names_and_path_crossing() -> Result<()> {
        for name in [
            "CON",
            "con.txt",
            "LPT2.log",
            "COM¹",
            "trailing.",
            "trailing ",
            "a:b",
            ".",
            "..",
        ] {
            assert!(FileName::parse(name).is_err(), "{name}");
        }
        for path in [
            "../secret",
            "x/../secret",
            "/absolute",
            "C:/data",
            "a\\b",
            "a//b",
        ] {
            assert!(LocalPath::parse(path).is_err(), "{path}");
        }
        assert_eq!(FileName::parse("报告 1.PDF")?.as_str(), "报告 1.PDF");
        Ok(())
    }
}
