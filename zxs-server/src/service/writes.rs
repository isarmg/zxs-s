use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
};

use super::{Error, FolderPath, Result, TxId};

#[derive(Default)]
pub(super) struct Writes {
    state: Mutex<Active>,
    completed: Condvar,
}

#[derive(Default)]
struct Active {
    writes: HashMap<TxId, Vec<String>>,
    frozen: HashMap<String, FolderOperation>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FolderOperation {
    Move,
    Recycle,
}

pub(super) struct WritePermit {
    owner: Arc<Writes>,
    id: TxId,
}

pub(super) struct FrozenFolder<'a> {
    owner: &'a Writes,
    path: String,
}

impl Writes {
    pub(super) fn admit(self: &Arc<Self>, id: &TxId, paths: &[&FolderPath]) -> Result<WritePermit> {
        let paths: Vec<_> = paths.iter().map(|path| path.key()).collect();
        let mut state = self.state.lock().map_err(|_| Error::Recovery)?;
        if let Some(folder) = state
            .frozen
            .keys()
            .find(|folder| paths.iter().any(|path| overlaps(folder, path)))
        {
            return Err(Error::FolderMoving(format!(
                "folder {folder} is moving across disks or being recycled. Retry after the operation completes"
            )));
        }
        state.writes.insert(id.clone(), paths);
        Ok(WritePermit {
            owner: Arc::clone(self),
            id: id.clone(),
        })
    }

    pub(super) fn freeze(
        &self,
        folder: &FolderPath,
        operation: FolderOperation,
    ) -> Result<FrozenFolder<'_>> {
        let key = folder.key();
        let mut state = self.state.lock().map_err(|_| Error::Recovery)?;
        if state.frozen.iter().any(|(path, active)| {
            overlaps(path, &key) || (*active == FolderOperation::Move && operation == *active)
        }) {
            return Err(Error::FolderMoving(
                "another folder is being moved or recycled. Retry after the operation completes"
                    .into(),
            ));
        }
        state.frozen.insert(key.clone(), operation);
        let frozen = FrozenFolder {
            owner: self,
            path: key,
        };
        // Existing writers can still acquire the publication lock while this waits.
        drop(
            self.completed
                .wait_while(state, |active| {
                    active
                        .writes
                        .values()
                        .flatten()
                        .any(|path| overlaps(&frozen.path, path))
                })
                .map_err(|_| Error::Recovery)?,
        );
        Ok(frozen)
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.owner.state.lock() {
            let paths = state.writes.remove(&self.id);
            if paths.is_some_and(|paths| {
                state
                    .frozen
                    .keys()
                    .any(|folder| paths.iter().any(|path| overlaps(folder, path)))
            }) {
                self.owner.completed.notify_all();
            }
        }
    }
}

impl Drop for FrozenFolder<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.owner.state.lock() {
            state.frozen.remove(&self.path);
        }
    }
}

fn overlaps(a: &str, b: &str) -> bool {
    fn contains(parent: &str, path: &str) -> bool {
        path.strip_prefix(parent.trim_end_matches('/'))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
    contains(a, b) || contains(b, a)
}
