use std::fmt::{Display, Formatter};
use std::num::NonZeroU32;

use serde::{Deserialize, Deserializer, Serialize};

use super::{Result, invalid};

macro_rules! text_type {
    ($name:ident, $validate:expr, $message:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
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

fn component(value: &str) -> bool {
    (1..=255).contains(&value.len())
        && !matches!(value, "." | "..")
        && !value
            .chars()
            .any(|c| matches!(c, '/' | '\\') || c.is_control())
}

fn folder(value: &str) -> bool {
    value == "/"
        || value
            .strip_prefix('/')
            .is_some_and(|tail| tail.split('/').all(component))
}

text_type!(ContainerId, |v| hex(v, 32), "invalid container ID");
text_type!(OpId, |v| hex(v, 32), "invalid operation ID");
text_type!(RecycleId, |v| hex(v, 32), "invalid recycle ID");
text_type!(Blake3Hash, |v| hex(v, 64), "invalid BLAKE3 hash");
text_type!(SessionToken, |v| hex(v, 64), "invalid session token");
text_type!(FolderPath, folder, "invalid folder path");
text_type!(FileName, component, "invalid file name");

impl FolderPath {
    pub fn child(&self, name: &str) -> Result<Self> {
        Self::parse(format!("{}/{}", self.0.trim_end_matches('/'), name))
    }
}

impl FileName {
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    pub fn payload_file(&self) -> String {
        let extension = self.0.rsplit_once('.').map(|(_, ext)| ext);
        match extension {
            Some(ext)
                if (1..=10).contains(&ext.len())
                    && ext
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) =>
            {
                format!("payload.{ext}")
            }
            _ => "payload.bin".into(),
        }
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

pub fn normalize_tags(mut tags: Vec<String>) -> Result<Vec<String>> {
    if tags.len() > 128
        || tags
            .iter()
            .any(|tag| tag.is_empty() || tag.len() > 255 || tag.chars().any(char::is_control))
    {
        return Err(invalid("invalid tags"));
    }
    tags.sort_unstable();
    tags.dedup();
    Ok(tags)
}
