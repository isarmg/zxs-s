use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::{ContainerId, Error, FileName, OpId, Result, VersionNum, invalid};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocalPath(String);

impl LocalPath {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value == "." {
            return Ok(Self(value));
        }
        if value.len() > 4096
            || value
                .split('/')
                .any(|segment| portable_component(segment).is_err())
        {
            return Err(invalid(format!("invalid local path: {value}")));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
        portable_component(name)?;
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

/// # Errors
///
/// Returns an error if the name cannot be used safely on supported platforms.
pub fn portable_component(name: &str) -> Result<FileName> {
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|tail| {
                matches!(
                    tail,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
    if reserved || name.ends_with(['.', ' ']) || name.contains([':', '*', '?', '<', '>', '|', '"'])
    {
        return Err(invalid(format!("path component is not portable: {name}")));
    }
    FileName::parse(name)
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub path: LocalPath,
    pub container_id: ContainerId,
    pub version: VersionNum,
    pub endpoint: String,
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingCommit {
    pub op_id: OpId,
    pub path: LocalPath,
    pub endpoint: String,
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Clean,
    Modified,
    Missing,
    Pending,
}

#[derive(Serialize)]
pub struct Status {
    pub path: LocalPath,
    pub container_id: Option<ContainerId>,
    pub version: Option<VersionNum>,
    pub state: FileState,
    pub op_id: Option<OpId>,
}

#[cfg(test)]
mod tests {
    use crate::domain::Result;

    use super::{LocalPath, portable_component};

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
            assert!(portable_component(name).is_err(), "{name}");
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
        assert_eq!(portable_component("报告 1.PDF")?.as_str(), "报告 1.PDF");
        Ok(())
    }
}
