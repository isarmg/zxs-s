use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex, MutexGuard},
};

use rayon::prelude::*;

use crate::{
    domain::{
        Blake3Hash, Container, ContainerId, Error, Folder, FolderPath, Result, Snapshot,
        VersionNum, invalid,
    },
    transport::Remote,
    zip_bundle::zip_stem,
};

use super::Catalog;

impl Catalog<'_> {
    pub(super) fn projection_conflicts(
        &self,
        containers: &[Container],
        bundles: &[ContainerId],
        direct: Option<(&FolderPath, &[Folder])>,
    ) -> Result<Vec<ContainerId>> {
        let mut conflicts = Vec::new();
        let mut known = HashMap::<FolderPath, Vec<Folder>>::new();
        for container in containers
            .iter()
            .filter(|container| bundles.contains(&container.container_id))
        {
            let children = if let Some((parent, folders)) = direct
                && parent == &container.folder
            {
                folders
            } else {
                use std::collections::hash_map::Entry;
                match known.entry(container.folder.clone()) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(self.children(&container.folder)?),
                }
            };
            if super::folders::matching_projection(children, &container.folder, &container.name)
                .is_some()
            {
                conflicts.push(container.container_id.clone());
            }
        }
        Ok(conflicts)
    }

    pub(super) fn classify<'c>(
        &self,
        containers: impl IntoIterator<Item = &'c Container>,
        live: bool,
    ) -> Result<Vec<ContainerId>> {
        let mut bundles = Vec::new();
        let mut unknown = Vec::new();
        for container in containers {
            if zip_stem(container.name.as_str()).is_none() {
                continue;
            }
            match self.known_bundle(container)? {
                Some(true) => bundles.push(container.container_id.clone()),
                None if live => unknown.push(container),
                _ => {}
            }
        }
        if unknown.is_empty() {
            return Ok(bundles);
        }
        let remote = self.remote;
        let detected = PROBES
            .as_ref()
            .map_err(|error| invalid(error.to_string()))?
            .install(|| {
                unknown
                    .into_par_iter()
                    .map(|container| {
                        probe(remote, container)
                            .or_else(|error| match error {
                                Error::Remote { status: 404, .. } => Ok(false),
                                error => Err(error),
                            })
                            .map(|bundle| bundle.then(|| container.container_id.clone()))
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
        bundles.extend(detected.into_iter().flatten());
        Ok(bundles)
    }

    pub(super) fn known_bundle(&self, container: &Container) -> Result<Option<bool>> {
        if let Some(index) = &self.index
            && let Some(track) = index.track_for_container(&container.container_id)?
            && track.version == container.latest_version
            && track.restored_from.is_none()
            && track.baseline_hash().is_some()
            && track.baseline_hash() == container.latest_blake3.as_ref()
            && matches!(track.snapshot, Snapshot::Bundle(_))
        {
            return Ok(Some(true));
        }
        let Some(key) = Key::new(self.remote, container) else {
            return Ok(None);
        };
        Ok(cache()?.items.get(&key).copied())
    }

    pub(super) fn probe_latest(&self, container: &Container) -> Result<bool> {
        PROBES
            .as_ref()
            .map_err(|error| invalid(error.to_string()))?
            .install(|| probe(self.remote, container))
    }
}

fn probe(remote: &Remote, container: &Container) -> Result<bool> {
    let key = Key::new(remote, container);
    if let Some(key) = &key
        && let Some(&bundle) = cache()?.items.get(key)
    {
        return Ok(bundle);
    }
    let bundle = remote.probe_bundle(
        &container.container_id,
        container.latest_version,
        container.latest_blake3.as_ref(),
    )?;
    if let Some(key) = key {
        cache()?.record(key, bundle);
    }
    Ok(bundle)
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Key {
    endpoint: String,
    id: ContainerId,
    version: VersionNum,
    blake3: Blake3Hash,
}

impl Key {
    fn new(remote: &Remote, container: &Container) -> Option<Self> {
        Some(Self {
            endpoint: remote.endpoint().into(),
            id: container.container_id.clone(),
            version: container.latest_version,
            blake3: container.latest_blake3.clone()?,
        })
    }
}

#[derive(Default)]
struct Cache {
    items: HashMap<Key, bool>,
    order: VecDeque<Key>,
}

impl Cache {
    fn record(&mut self, key: Key, bundle: bool) {
        if self.items.insert(key.clone(), bundle).is_none() {
            self.order.push_back(key);
        }
        if self.items.len() > 4096
            && let Some(oldest) = self.order.pop_front()
        {
            self.items.remove(&oldest);
        }
    }
}

fn cache() -> Result<MutexGuard<'static, Cache>> {
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);
    CACHE
        .lock()
        .map_err(|_| invalid("ZIP classification cache is poisoned"))
}

static PROBES: LazyLock<std::result::Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>> =
    LazyLock::new(|| rayon::ThreadPoolBuilder::new().num_threads(4).build());
