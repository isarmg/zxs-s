use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, ContainerId, LocalPath, RemoteBundle, Result, VersionNum, invalid, media_type,
};

use super::{
    Connection,
    cache::{self, CachedDocument},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowseBundle {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    directory: LocalPath,
    cursor: Option<String>,
}

impl BrowseBundle {
    pub(super) fn run(self) -> Result<Value> {
        let bundle = RemoteBundle::open(&self.connection.open()?, &self.id, self.version)?;
        Ok(serde_json::to_value(
            bundle.browse(&self.directory, self.cursor.as_deref())?,
        )?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenMember {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    path: LocalPath,
    cache: Option<PathBuf>,
    output: Option<PathBuf>,
}

impl OpenMember {
    pub(super) fn run(self) -> Result<Value> {
        let mut bundle = RemoteBundle::open(&self.connection.open()?, &self.id, self.version)?;
        let member = bundle.member(&self.path)?;
        let mime = media_type(&member.name);
        let file = match (self.cache, self.output) {
            (Some(cache), None) => {
                let key = member.cache_key.clone();
                let name = member.name.clone();
                let document = CachedDocument {
                    key: &key,
                    name: &name,
                    size: member.size,
                };
                cache::materialize(&cache, &document, |path| member.write_new(path))?
            }
            (None, Some(output)) => {
                member.write_new(&output)?;
                output
            }
            _ => return Err(invalid("指定缓存目录或目标文件路径")),
        };
        Ok(json!({ "path": file, "type": mime }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Download {
    connection: Connection,
    id: ContainerId,
    version: VersionNum,
    output: PathBuf,
}

impl Download {
    pub(super) fn run(self) -> Result<Value> {
        let remote = self.connection.open()?;
        let (_, version) = remote.version(&self.id, Some(self.version))?;
        remote.download_new(&self.id, &version, &self.output)?;
        Ok(json!({}))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenFile {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    cache: PathBuf,
    #[serde(default)]
    export: bool,
}

impl OpenFile {
    pub(super) fn run(self) -> Result<Value> {
        let remote = self.connection.open()?;
        let (mut container, version) = remote.version(&self.id, self.version)?;
        container.latest_version = version.version;
        let bundle = Catalog::open(&remote, None)?.is_bundle(&container)?;
        if bundle && !self.export {
            return Ok(json!({ "bundle": true }));
        }
        let mime = media_type(&version.name_at_commit);
        if mime.starts_with("video/") || mime.starts_with("audio/") {
            let (url, authorization) = remote.media_request(&self.id, version.version)?;
            Ok(json!({ "url": url, "headers": { "Authorization": authorization }, "type": mime }))
        } else {
            let path = cache::document(&remote, &self.cache, &self.id, &version)?;
            Ok(json!({ "path": path, "type": mime, "bundle": bundle }))
        }
    }
}
