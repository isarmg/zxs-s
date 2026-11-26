mod auth;
mod changes;
mod folders;
mod maintenance;
mod publication;
mod uploads;
mod writes;

use std::{
    collections::HashSet,
    fs::File,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    domain::{
        Blake3Hash, Change, Commit, Container, ContainerId, ContainerPage, Detail, Error, FileName,
        Filter, FolderPath, NewContainer, NewVersion, Patch, Pending, RecycleFilter, RecycleId,
        RecycleInfo, RecyclePage, Recycled, Restore, Result, SessionToken, TxId, Version,
        VersionNum, invalid, normalize_tags,
    },
    store::{
        Config, StageGuard, Store,
        db::{self, Db},
    },
};

pub use auth::password_hash;
pub use maintenance::verify;
pub use uploads::{UploadHeaders, UploadIntent};

pub struct State {
    store: Store,
    db: Db,
    config: Config,
    publication: RwLock<()>,
    writes: Arc<writes::Writes>,
    password_check: Mutex<()>,
    sessions: Mutex<HashSet<Blake3Hash>>,
    needs_recovery: AtomicBool,
    transfers: Arc<tokio::sync::Semaphore>,
    _disk_lock: File,
}

impl State {
    pub fn initialize(mut store: Store, mut config: Config, lock: File) -> Result<()> {
        store.extend_registrations(&mut config)?;
        store.bind(&config);
        let state = Self::open(store, config, lock, true)?;
        state.store.save_config(&state.config)
    }

    pub fn open(store: Store, config: Config, lock: File, rebuild: bool) -> Result<Self> {
        store.prepare()?;
        let (db, missing) = open_index(&store, rebuild)?;
        let state = Self {
            store,
            db,
            config,
            publication: RwLock::default(),
            writes: Arc::default(),
            password_check: Mutex::new(()),
            sessions: Mutex::default(),
            needs_recovery: AtomicBool::new(false),
            transfers: Arc::new(tokio::sync::Semaphore::new(8)),
            _disk_lock: lock,
        };
        state.recover(rebuild || missing, !missing)?;
        Ok(state)
    }

    pub fn sync_disks(mut self) -> Result<bool> {
        if !self.store.extend_registrations(&mut self.config)? {
            return Ok(false);
        }
        self.store.check_indexed_paths()?;
        self.store.bind(&self.config);
        self.store.prepare()?;
        let stage = self.store.stage_plan(&new_tx_id()?)?;
        let index = stage.path.join("index.sqlite");
        let (db, _) = Db::open(&index)?;
        maintenance::rebuild(&self.store, &db)?;
        drop(db);
        drop(self.db);

        // An absent index is rebuilt from whichever registration survived a restart.
        self.store.discard_index()?;
        self.store.save_config(&self.config)?;
        if let Err(error) = self.store.install_index(&index) {
            tracing::warn!(%error, "index will be rebuilt on startup");
        }
        Ok(true)
    }

    pub async fn transfer(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.transfers)
            .acquire_owned()
            .await
            .map_err(|_| Error::Recovery)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn healthy(&self) -> Result<()> {
        if self.needs_recovery.load(Ordering::Acquire) {
            Err(Error::Recovery)
        } else {
            Ok(())
        }
    }

    fn indexed_history(&self, container: &Container) -> Result<Detail> {
        self.db
            .detail(&container.id)?
            .filter(|detail| &detail.container == container)
            .ok_or_else(|| Error::Conflict("container changed during storage preparation".into()))
    }

    fn mark_on_error<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.needs_recovery.store(true, Ordering::Release);
        }
        result
    }

    fn read_publication<T>(&self, call: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.publication.read().map_err(|_| Error::Recovery)?;
        self.healthy()?;
        call()
    }

    fn serialize_publication<T>(&self, call: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.publication.write().map_err(|_| Error::Recovery)?;
        self.healthy()?;
        call()
    }

    fn container(&self, id: &ContainerId) -> Result<Container> {
        self.db
            .container(id)?
            .ok_or(Error::Missing("container not found"))
    }

    fn available(
        &self,
        folder: &FolderPath,
        name: &FileName,
        except: Option<&ContainerId>,
    ) -> Result<()> {
        self.store.paths.data_disk(folder)?;
        self.db.check_container(folder, name, except)
    }
}

fn now() -> u64 {
    std::time::UNIX_EPOCH
        .elapsed()
        .unwrap_or_default()
        .as_secs()
}

fn digest(bytes: &[u8]) -> Result<Blake3Hash> {
    Blake3Hash::parse(blake3::hash(bytes).to_hex().to_string())
}

fn new_tx_id() -> Result<TxId> {
    TxId::parse(hex(random()?))
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::Config(format!("OS random source: {error}")))?;
    Ok(bytes)
}

fn hex(bytes: [u8; 16]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn new_container_id() -> Result<ContainerId> {
    let mut bytes = random::<16>()?;
    let milliseconds = std::time::UNIX_EPOCH
        .elapsed()
        .unwrap_or_default()
        .as_millis();
    bytes[..6].copy_from_slice(&milliseconds.to_be_bytes()[10..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    ContainerId::parse(hex(bytes))
}

fn open_index(store: &Store, rebuild: bool) -> Result<(Db, bool)> {
    match Db::open(&store.paths.index()) {
        Err(Error::Index(rusqlite::Error::SqliteFailure(error, _)))
            if rebuild
                && matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) =>
        {
            store.discard_index()?;
            Db::open(&store.paths.index())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test_support::TestDir;

    use super::{Result, open_index};

    #[test]
    fn rebuilding_replaces_a_corrupt_disposable_index() -> Result<()> {
        let root = TestDir::create()?;
        let store = root.store()?;
        fs::write(store.paths.index(), b"corrupt index\n".repeat(128))?;
        assert!(open_index(&store, false).is_err());
        let (db, missing) = open_index(&store, true)?;
        assert!(missing);
        db.rebuild(|_| Ok(()))?;
        Ok(())
    }
}
