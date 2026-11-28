use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    ContainerId, LocalBundle, LocalPath, Result, Version, VersionNum, invalid, media_type, zip_stem,
};

use super::{Connection, Transfer, cache};

#[derive(PartialEq, Eq)]
struct BundleKey {
    connection: Connection,
    cache: PathBuf,
    id: ContainerId,
    version: Option<VersionNum>,
}

impl BundleKey {
    fn new(
        connection: Connection,
        cache: PathBuf,
        id: ContainerId,
        version: Option<VersionNum>,
    ) -> Self {
        Self {
            connection,
            cache,
            id,
            version,
        }
    }
}

struct OpenedBundle {
    key: BundleKey,
    bundle: LocalBundle,
}

static OPENED: Mutex<Option<OpenedBundle>> = Mutex::new(None);

pub(super) fn close_bundle() -> Result<()> {
    *OPENED.lock().map_err(|_| invalid("目录工作区不可用"))? = None;
    Ok(())
}

fn remember_bundle<T>(
    mut key: BundleKey,
    path: &Path,
    version: Version,
    read: impl FnOnce(&mut LocalBundle) -> Result<T>,
) -> Result<Option<T>> {
    key.version = Some(version.version);
    let Some(mut bundle) = LocalBundle::open(path, key.id.clone(), version)? else {
        return Ok(None);
    };
    let result = read(&mut bundle)?;
    *OPENED.lock().map_err(|_| invalid("目录工作区不可用"))? = Some(OpenedBundle { key, bundle });
    Ok(Some(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowseBundle {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    directory: LocalPath,
    cursor: Option<String>,
    cache: PathBuf,
}

impl BrowseBundle {
    pub(super) fn run(self, transfer: Transfer) -> Result<Value> {
        let key = BundleKey::new(self.connection, self.cache, self.id, self.version);
        let page = with_bundle(key, transfer, |bundle| {
            bundle.browse(&self.directory, self.cursor.as_deref())
        })?;
        Ok(serde_json::to_value(page)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenMember {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    path: LocalPath,
    cache: PathBuf,
}

impl OpenMember {
    pub(super) fn run(self, transfer: Transfer) -> Result<Value> {
        let members_cache = self.cache.join("bundle-members");
        let key = BundleKey::new(self.connection, self.cache, self.id, self.version);
        with_bundle(key, transfer, |bundle| {
            let member = bundle.member(&self.path)?;
            let mime = media_type(&member.name);
            let key = member.cache_key.clone();
            let name = member.name.clone();
            let file = cache::materialize(&members_cache, &key, &name, None, |path| {
                member.write_new(path)
            })?;
            Ok(json!({ "path": file, "type": mime }))
        })
    }
}

fn with_bundle<T>(
    key: BundleKey,
    transfer: Transfer,
    read: impl FnOnce(&mut LocalBundle) -> Result<T>,
) -> Result<T> {
    if key.version.is_some() {
        let mut opened = OPENED.lock().map_err(|_| invalid("目录工作区不可用"))?;
        if let Some(entry) = opened.as_mut().filter(|entry| entry.key == key) {
            return read(&mut entry.bundle);
        }
    }
    let remote = key.connection.transfer(transfer)?;
    let (container, version) = remote.version(&key.id, key.version)?;
    let name = version.preview_name(&container);
    if zip_stem(name.as_str()).is_none() {
        return Err(invalid("该版本不是目录工作区"));
    }
    let path = cache::document(&remote, &key.cache, &key.id, &version, name)?;
    remember_bundle(key, &path, version, read)?.ok_or_else(|| invalid("该版本不是目录工作区"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenFile {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    cache: PathBuf,
}

impl OpenFile {
    pub(super) fn run(self, transfer: Transfer) -> Result<Value> {
        let remote = self.connection.transfer(transfer)?;
        let (container, version) = remote.version(&self.id, self.version)?;
        let name = version.preview_name(&container);
        let zip = zip_stem(name.as_str()).is_some();
        let mime = media_type(&version.name_at_commit);
        if !zip && (mime.starts_with("video/") || mime.starts_with("audio/")) {
            let (url, authorization) = remote.media_request(&self.id, version.version)?;
            return Ok(
                json!({ "url": url, "headers": { "Authorization": authorization }, "type": mime }),
            );
        }
        let path = cache::document(&remote, &self.cache, &self.id, &version, name)?;
        let mime = media_type(name);
        if zip {
            let key = BundleKey::new(self.connection, self.cache, self.id, self.version);
            if let Some(page) = remember_bundle(key, &path, version, |bundle| {
                bundle.browse(&LocalPath::try_from(".".to_owned())?, None)
            })? {
                let mut reply = serde_json::to_value(page)?;
                reply["bundle"] = json!(true);
                return Ok(reply);
            }
        }
        Ok(json!({ "path": path, "type": mime, "bundle": false }))
    }
}
