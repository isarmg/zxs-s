mod auth;
mod changes;
mod maintenance;
mod uploads;

use std::{
    collections::HashSet,
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    domain::{
        Blake3Hash, Change, Commit, Container, ContainerId, ContainerPage, Detail, Error, FileName,
        Filter, FolderPath, NewContainer, NewVersion, OpId, Patch, Pending, Receipt, Record,
        RecycleFilter, RecycleId, RecycleInfo, RecyclePage, Recycled, Restore, Result,
        SessionEffect, SessionToken, Version, VersionNum, invalid, normalize_tags,
    },
    store::{
        Config, Layout, StageGuard, Store,
        db::{self, Db},
    },
};

pub use auth::{password_hash, session_key};
pub use uploads::{BeginUpload, UploadIntent};

pub struct State {
    store: Store,
    db: Db,
    config: Config,
    generation: Blake3Hash,
    container_locks: [Mutex<()>; 64],
    publication: Mutex<()>,
    password_check: Mutex<()>,
    operations: Arc<Mutex<HashSet<OpId>>>,
    needs_recovery: AtomicBool,
    transfers: Arc<tokio::sync::Semaphore>,
    _disk_lock: File,
}

pub enum Admission {
    Replay(Receipt),
    New(Lease),
}

pub struct Lease {
    id: OpId,
    active: Arc<Mutex<HashSet<OpId>>>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.id);
        }
    }
}

impl State {
    pub fn open(store: Store, config: Config, lock: File, rebuild: bool) -> Result<Self> {
        store.prepare()?;
        let (db, missing) = open_index(&store, rebuild)?;
        let generation = digest(config.session_key.as_str().as_bytes())?;
        let state = Self {
            store,
            db,
            config,
            generation,
            container_locks: std::array::from_fn(|_| Mutex::new(())),
            publication: Mutex::new(()),
            password_check: Mutex::new(()),
            operations: Arc::default(),
            needs_recovery: AtomicBool::new(false),
            transfers: Arc::new(tokio::sync::Semaphore::new(8)),
            _disk_lock: lock,
        };
        state.recover(rebuild || missing)?;
        Ok(state)
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

    fn recover(&self, rebuild: bool) -> Result<()> {
        let pending = self.store.recover_changes()?;
        let auth = self.store.recover_auth()?;
        for transaction in &pending {
            self.apply_files(transaction)?;
            if !rebuild {
                self.apply_index(transaction)?;
            }
        }
        if let Some(record) = &auth {
            self.store.store_record(record)?;
            if !rebuild {
                self.index_auth(record)?;
            }
        }
        self.store.clean_stages()?;
        if rebuild {
            self.rebuild()?;
        }
        for transaction in &pending {
            self.store.end(transaction.change.container_id())?;
        }
        if auth.is_some() {
            self.store.end_auth()?;
        }
        Ok(())
    }

    fn healthy(&self) -> Result<()> {
        if self.needs_recovery.load(Ordering::Acquire) {
            Err(Error::Recovery)
        } else {
            Ok(())
        }
    }

    fn mark_on_error<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.needs_recovery.store(true, Ordering::Release);
        }
        result
    }

    fn with_container<T>(&self, id: &ContainerId, call: impl FnOnce() -> Result<T>) -> Result<T> {
        self.healthy()?;
        let mut hash = DefaultHasher::new();
        id.hash(&mut hash);
        let index = usize::from(hash.finish().to_ne_bytes()[0]) % self.container_locks.len();
        let _guard = self.container_locks[index]
            .lock()
            .map_err(|_| Error::Recovery)?;
        self.healthy()?;
        call()
    }

    fn mutate<T>(&self, id: &ContainerId, call: impl FnOnce() -> Result<T>) -> Result<T> {
        self.with_container(id, || self.serialize_publication(call))
    }

    fn serialize_publication<T>(&self, call: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.publication.lock().map_err(|_| Error::Recovery)?;
        self.healthy()?;
        call()
    }

    fn admit(&self, op: &OpId, fingerprint: &Blake3Hash) -> Result<Admission> {
        self.healthy()?;
        let mut active = self.operations.lock().map_err(|_| Error::Recovery)?;
        if let Some(record) = self.db.record(op)? {
            return if &record.fingerprint == fingerprint {
                Ok(Admission::Replay(record.receipt))
            } else {
                Err(Error::Reused)
            };
        }
        if !active.insert(op.clone()) {
            return Err(Error::Conflict("operation is still in progress".into()));
        }
        Ok(Admission::New(Lease {
            id: op.clone(),
            active: Arc::clone(&self.operations),
        }))
    }

    fn transact(&self, pending: &Pending, stage: Option<&mut StageGuard>) -> Result<()> {
        // A successful intent fsync transfers ownership of staging to recovery.
        if let Err(error) = self.store.start(pending) {
            if self
                .store
                .paths
                .pending(pending.change.container_id())
                .exists()
            {
                if let Some(stage) = stage {
                    stage.journaled();
                }
                return self.mark_on_error(Err(error));
            }
            return Err(error);
        }
        self.mark_on_error((|| {
            let mut stage = stage;
            if let Some(guard) = &mut stage {
                guard.journaled();
            }
            self.apply_files(pending)?;
            self.apply_index(pending)?;
            if let Some(guard) = stage {
                guard.finish()?;
            }
            self.store.end(pending.change.container_id())
        })())
    }

    fn apply_files(&self, pending: &Pending) -> Result<()> {
        self.store.apply(&pending.change)?;
        if let Some(record) = &pending.record {
            self.store.store_record(record)?;
        }
        Ok(())
    }

    fn apply_index(&self, pending: &Pending) -> Result<()> {
        let restored = if let Change::Restore { container, .. } = &pending.change {
            Some(Store::snapshot(&self.store.paths.container(&container.id))?)
        } else {
            None
        };
        self.db.write(|tx| {
            db::apply_change(tx, &pending.change, restored.as_ref())?;
            if let Some(record) = &pending.record {
                db::put_record(tx, record)?;
            }
            Ok(())
        })
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
        if self.db.name_taken(folder, name, except)? {
            Err(Error::Conflict(
                "a container with this folder and name already exists".into(),
            ))
        } else {
            Ok(())
        }
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

fn fingerprint(input: &impl serde::Serialize) -> Result<Blake3Hash> {
    digest(&serde_json::to_vec(input)?)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    use std::io::Read;
    let mut bytes = [0; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
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

    use super::{Layout, Result, Store, open_index};

    #[test]
    fn rebuilding_replaces_a_corrupt_disposable_index() -> Result<()> {
        let root = TestDir::create()?;
        let store = Store {
            paths: Layout {
                data: root.0.join("data"),
                recycle: root.0.join("recycle"),
            },
        };
        store.prepare()?;
        fs::write(store.paths.index(), b"corrupt index\n".repeat(128))?;
        assert!(open_index(&store, false).is_err());
        let (db, missing) = open_index(&store, true)?;
        assert!(missing);
        db.rebuild(|_| Ok(()))?;
        Ok(())
    }
}
