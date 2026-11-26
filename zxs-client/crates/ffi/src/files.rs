use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    ContainerId, LocalBundle, LocalPath, Result, Version, VersionNum, invalid, media_type, zip_stem,
};

use super::{
    Connection,
    cache::{self, CachedDocument},
};

type SharedBundle = Arc<Mutex<LocalBundle>>;

#[derive(PartialEq, Eq)]
struct BundleKey {
    connection: Connection,
    cache: PathBuf,
    id: ContainerId,
    version: VersionNum,
}

impl BundleKey {
    fn new(connection: &Connection, cache: &Path, id: ContainerId, version: VersionNum) -> Self {
        Self {
            connection: connection.clone(),
            cache: cache.into(),
            id,
            version,
        }
    }
}

struct OpenedBundle {
    key: BundleKey,
    bundle: SharedBundle,
}

static OPENED: Mutex<Option<OpenedBundle>> = Mutex::new(None);

pub(super) fn close_bundle() -> Result<()> {
    *OPENED.lock().map_err(|_| invalid("目录工作区不可用"))? = None;
    Ok(())
}

fn remember_bundle(key: BundleKey, path: &Path, version: Version) -> Result<Option<SharedBundle>> {
    let mut opened = OPENED.lock().map_err(|_| invalid("目录工作区不可用"))?;
    if opened.as_ref().is_some_and(|entry| entry.key == key) {
        *opened = None;
    }
    let Some(bundle) = LocalBundle::open(path, key.id.clone(), version)? else {
        return Ok(None);
    };
    let bundle = Arc::new(Mutex::new(bundle));
    *opened = Some(OpenedBundle {
        key,
        bundle: Arc::clone(&bundle),
    });
    Ok(Some(bundle))
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
    pub(super) fn run(self) -> Result<Value> {
        let bundle = open_bundle(&self.connection, self.id, self.version, &self.cache)?;
        let bundle = bundle.lock().map_err(|_| invalid("目录工作区不可用"))?;
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
    cache: PathBuf,
}

impl OpenMember {
    pub(super) fn run(self) -> Result<Value> {
        let bundle = open_bundle(&self.connection, self.id, self.version, &self.cache)?;
        let mut bundle = bundle.lock().map_err(|_| invalid("目录工作区不可用"))?;
        let member = bundle.member(&self.path)?;
        let mime = media_type(&member.name);
        let key = member.cache_key.clone();
        let name = member.name.clone();
        let document = CachedDocument {
            key: &key,
            name: &name,
            size: member.size,
            digest: None,
        };
        let file = cache::materialize(&self.cache.join("bundle-members"), &document, |path| {
            member.write_new(path)
        })?;
        Ok(json!({ "path": file, "type": mime }))
    }
}

fn open_bundle(
    connection: &Connection,
    id: ContainerId,
    number: Option<VersionNum>,
    cache: &Path,
) -> Result<SharedBundle> {
    if let Some(version) = number {
        let key = BundleKey::new(connection, cache, id.clone(), version);
        let opened = OPENED.lock().map_err(|_| invalid("目录工作区不可用"))?;
        if let Some(entry) = opened.as_ref().filter(|entry| entry.key == key) {
            return Ok(Arc::clone(&entry.bundle));
        }
    }
    let remote = connection.open()?;
    let (container, version) = remote.version(&id, number)?;
    if zip_stem(container.name.as_str()).is_none() {
        return Err(invalid("该版本不是目录工作区"));
    }
    let path = cache::document(&remote, cache, &id, &version)?;
    let key = BundleKey::new(connection, cache, id, version.version);
    remember_bundle(key, &path, version)?.ok_or_else(|| invalid("该版本不是目录工作区"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Download {
    connection: Connection,
    id: ContainerId,
    version: Option<VersionNum>,
    output: PathBuf,
}

impl Download {
    pub(super) fn run(self) -> Result<Value> {
        let remote = self.connection.open()?;
        let (container, version) = remote.version(&self.id, self.version)?;
        remote.download_new(&self.id, &version, &self.output)?;
        Ok(json!({ "container": container, "versions": [version] }))
    }
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
    pub(super) fn run(self) -> Result<Value> {
        let remote = self.connection.open()?;
        let (container, version) = remote.version(&self.id, self.version)?;
        let zip = zip_stem(container.name.as_str()).is_some();
        let mime = media_type(&version.name_at_commit);
        if !zip && (mime.starts_with("video/") || mime.starts_with("audio/")) {
            let (url, authorization) = remote.media_request(&self.id, version.version)?;
            return Ok(
                json!({ "url": url, "headers": { "Authorization": authorization }, "type": mime }),
            );
        }
        let path = cache::document(&remote, &self.cache, &self.id, &version)?;
        if zip {
            let key = BundleKey::new(&self.connection, &self.cache, self.id, version.version);
            if let Some(bundle) = remember_bundle(key, &path, version)? {
                let bundle = bundle.lock().map_err(|_| invalid("目录工作区不可用"))?;
                let mut reply = serde_json::to_value(
                    bundle.browse(&LocalPath::try_from(".".to_owned())?, None)?,
                )?;
                reply["bundle"] = json!(true);
                return Ok(reply);
            }
        }
        Ok(json!({ "path": path, "type": mime, "bundle": false }))
    }
}
