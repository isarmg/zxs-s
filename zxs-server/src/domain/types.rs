use std::fmt::{Display, Formatter};
use std::num::NonZeroU32;

use serde::{Deserialize, Deserializer, Serialize};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use super::{Result, invalid};

macro_rules! text_type {
    ($name:ident, $validate:expr, $message:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = normalized(value.into());
                if ($validate)(&value) {
                    Ok(Self(value))
                } else {
                    Err(invalid($message))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                Self::parse(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn normalized(value: String) -> String {
    if is_nfc(&value) {
        value
    } else {
        value.nfc().collect()
    }
}

fn component(value: &str) -> bool {
    (1..=255).contains(&value.len())
        && !value.ends_with(['.', ' '])
        && !value.eq_ignore_ascii_case(".zxs")
        && !value.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
        && !device_name(value)
}

fn device_name(value: &str) -> bool {
    let base = value
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

fn folder(value: &str) -> bool {
    value == "/"
        || (value.len() <= 1024
            && value
                .strip_prefix('/')
                .is_some_and(|tail| tail.split('/').all(component)))
}

text_type!(ContainerId, |v| hex(v, 32), "invalid container ID");
text_type!(TxId, |v| hex(v, 32), "invalid temporary transaction ID");
text_type!(RecycleId, |v| hex(v, 32), "invalid recycle ID");
text_type!(DiskId, |v| hex(v, 32), "invalid disk ID");
text_type!(DiskName, component, "invalid disk name");
text_type!(Blake3Hash, |v| hex(v, 64), "invalid BLAKE3 hash");
text_type!(SessionToken, |v| hex(v, 64), "invalid session token");
text_type!(FolderPath, folder, "invalid folder path");
text_type!(FileName, component, "invalid file name");

impl FolderPath {
    pub fn root() -> Self {
        Self("/".into())
    }

    pub fn child(&self, name: &str) -> Result<Self> {
        Self::parse(format!("{}/{}", self.0.trim_end_matches('/'), name))
    }

    pub fn parent(&self) -> Option<Self> {
        if self.0 == "/" {
            return None;
        }
        let (parent, _) = self.0.rsplit_once('/')?;
        Some(if parent.is_empty() {
            Self::root()
        } else {
            Self(parent.into())
        })
    }

    pub fn key(&self) -> String {
        self.0.to_lowercase().nfc().collect()
    }

    pub fn disk(&self) -> Result<DiskName> {
        let name = self
            .0
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        if name.is_empty() {
            return Err(invalid("select a registered data disk"));
        }
        DiskName::parse(name)
    }

    pub fn require_child(&self) -> Result<()> {
        if self.parent().is_none_or(|parent| parent == Self::root()) {
            return Err(invalid("disk roots are managed by init and sync-disks"));
        }
        Ok(())
    }
}

impl std::str::FromStr for FolderPath {
    type Err = super::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl DiskName {
    pub fn key(&self) -> String {
        self.0.to_lowercase().nfc().collect()
    }
}

impl FileName {
    pub fn key(&self) -> String {
        self.0.to_lowercase().nfc().collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionNum(NonZeroU32);

impl VersionNum {
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    pub fn new(value: u32) -> Result<Self> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or_else(|| invalid("invalid version number"))
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }

    pub fn next(self) -> Result<Self> {
        self.get()
            .checked_add(1)
            .ok_or_else(|| super::Error::Conflict("version number exhausted".into()))
            .and_then(Self::new)
    }
}

impl Display for VersionNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

pub fn normalize_tags(mut tags: Vec<String>) -> Vec<String> {
    tags.sort_unstable();
    tags.dedup();
    tags
}

#[cfg(test)]
mod tests {
    use crate::domain::Result;

    use super::{FileName, FolderPath, VersionNum};

    #[test]
    fn identities_reject_invalid_paths_and_version_overflow() -> Result<()> {
        for folder in ["", "docs", "/docs/", "/docs//x", "/docs/..", "/bad\\name"] {
            assert!(FolderPath::parse(folder).is_err(), "{folder}");
        }
        for file in ["", ".", "..", ".ZXS", "docs/a", "docs\\a", "bad\nname"] {
            assert!(FileName::parse(file).is_err(), "{file}");
        }
        assert_eq!(FileName::parse("Photo.JPG")?.key(), "photo.jpg");
        assert_eq!(FolderPath::parse("/")?.child("文档")?.as_str(), "/文档");
        assert!(VersionNum::new(0).is_err());
        assert!(VersionNum::new(u32::MAX)?.next().is_err());
        Ok(())
    }
}
