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
            /// # Errors
            ///
            /// Returns an error if the supplied text does not match the required format.
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = normalized(value.into());
                if ($validate)(&value) {
                    Ok(Self(value))
                } else {
                    Err(invalid($message))
                }
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = super::Error;
            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommitTarget {
    Tracked(ContainerId),
    SameName { same_name: ContainerId },
}
text_type!(TxId, |v| hex(v, 32), "invalid temporary transaction ID");
text_type!(RecycleId, |v| hex(v, 32), "invalid recycle ID");
text_type!(Blake3Hash, |v| hex(v, 64), "invalid BLAKE3 hash");
text_type!(SessionToken, |v| hex(v, 64), "invalid session token");
text_type!(FolderPath, folder, "invalid folder path");
text_type!(FileName, component, "invalid file name");
text_type!(DiskName, component, "invalid disk name");

impl FolderPath {
    #[must_use]
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

    pub(crate) fn root() -> Self {
        Self("/".into())
    }

    pub(crate) fn disk(&self) -> Result<DiskName> {
        if self == &Self::root() {
            return Err(invalid(
                "请将文件放入工作区内的数据盘目录，或通过 --folder /<数据盘> 指定提交位置",
            ));
        }
        DiskName::parse(self.0[1..].split('/').next().unwrap_or_default())
    }

    pub(crate) fn require_child(&self) -> Result<()> {
        if self.parent().is_none_or(|parent| parent == Self::root()) {
            return Err(invalid("此操作只可在数据盘内的子目录执行"));
        }
        Ok(())
    }
}

impl FileName {
    #[must_use]
    pub fn key(&self) -> String {
        file_name_key(&self.0)
    }
}

pub(crate) fn file_name_key(name: &str) -> String {
    normalized(name.to_lowercase())
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionNum(NonZeroU32);

impl VersionNum {
    /// # Errors
    ///
    /// Returns an error when the version number is zero.
    pub fn new(value: u32) -> Result<Self> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or_else(|| invalid("invalid version number"))
    }
}

impl Display for VersionNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl std::str::FromStr for VersionNum {
    type Err = super::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(
            value
                .parse()
                .map_err(|_| invalid("invalid version number"))?,
        )
    }
}
