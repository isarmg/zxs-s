use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
};

use rayon::prelude::*;

use super::{
    Change, ContainerId, File, FileName, FolderPath, Layout, NewContainer, Ordering, Pending,
    Result, SessionEffect, State, Store, Version, db, invalid, now, uploads::hash_file,
};

impl State {
    pub(super) fn rebuild(&self) -> Result<()> {
        let live = self.store.live_paths()?;
        let recycled = self.store.recycle_paths()?;
        let operations = self.store.operation_paths()?;
        let at = now();
        self.db.rebuild(|tx| {
            for path in &live {
                let detail = Store::snapshot(path)?;
                check_identity(path, detail.container.id.as_str())?;
                db::put_detail(tx, &detail)?;
            }
            for path in &recycled {
                let entry = Store::read_recycled(path)?;
                check_identity(path, entry.recycle.recycle_id.as_str())?;
                db::put_recycle(tx, &entry)?;
            }
            let mut logout = Vec::new();
            for path in &operations {
                let record = Store::read_record(path)?;
                if path.file_stem().and_then(|s| s.to_str()) != Some(record.receipt.op_id.as_str())
                {
                    return Err(invalid(format!(
                        "unexpected operation record {}",
                        path.display()
                    )));
                }
                db::put_record(tx, &record)?;
                match record.session {
                    Some(effect @ SessionEffect::Login { .. }) => {
                        db::put_session_effect(tx, &effect, &self.generation, at)?;
                    }
                    Some(effect @ SessionEffect::Logout { .. }) => logout.push(effect),
                    None => {}
                }
            }
            for effect in logout {
                db::put_session_effect(tx, &effect, &self.generation, at)?;
            }
            Ok(())
        })
    }

    pub fn verify(&self) -> Result<usize> {
        let live = self.store.live_paths()?;
        let recycled = self.store.recycle_paths()?;
        let count = AtomicUsize::new(0);
        live.par_iter()
            .try_for_each(|path| Self::verify_container(path, &count))?;
        recycled.par_iter().try_for_each(|path| {
            let entry = Store::read_recycled(path)?;
            check_identity(path, entry.recycle.recycle_id.as_str())?;
            Self::verify_container(&Layout::recycled_container(path), &count)
        })?;
        Ok(count.into_inner())
    }

    fn verify_container(path: &Path, count: &AtomicUsize) -> Result<()> {
        let detail = Store::snapshot(path)?;
        if path.file_name().and_then(|name| name.to_str()) != Some("container") {
            check_identity(path, detail.container.id.as_str())?;
        }
        for v in &detail.versions {
            let p = Layout::payload_file(&Layout::version(path, v.number), &v.payload_file);
            verify_payload(&p, v)?;
            count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn prune(&self, keep: usize, id: Option<ContainerId>) -> Result<usize> {
        if keep == 0 {
            return Err(invalid("keep-last must be at least 1"));
        }
        let paths = match id {
            Some(id) => {
                self.container(&id)?;
                vec![self.store.paths.container(&id)]
            }
            None => self.store.live_paths()?,
        };
        let mut removed = 0;
        for path in paths {
            let detail = Store::snapshot(&path)?;
            let to_remove = detail.versions.len().saturating_sub(keep);
            let id = detail.container.id;
            for chunk in detail.versions[..to_remove].chunks(4096) {
                let versions = chunk.iter().map(|v| v.number).collect();
                let pending = Pending {
                    change: Change::Prune {
                        container_id: id.clone(),
                        versions,
                    },
                    record: None,
                };
                self.transact(&pending, None)?;
            }
            removed += to_remove;
        }
        Ok(removed)
    }

    pub fn import(&self, source: &Path, folder: &FolderPath) -> Result<usize> {
        let mut items = Vec::new();
        collect(source, folder, &mut items)?;
        items
            .par_iter()
            .try_for_each(|(path, params)| self.import_payload(&File::open(path)?, params))?;
        Ok(items.len())
    }
}

fn check_identity(path: &Path, name: &str) -> Result<()> {
    if path.file_name().and_then(|n| n.to_str()) != Some(name)
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            != Some(&name[..2])
    {
        return Err(invalid(format!(
            "incorrect shard or ID: {}",
            path.display()
        )));
    }
    Ok(())
}

fn verify_payload(path: &Path, version: &Version) -> Result<()> {
    let (size, hash) = hash_file(File::open(path)?, io::sink())?;
    if size != version.size || hash.finalize().to_hex().as_str() != version.blake3.as_str() {
        return Err(invalid(format!(
            "payload verification failed: {}",
            path.display()
        )));
    }
    Ok(())
}

fn collect(
    dir: &Path,
    folder: &FolderPath,
    items: &mut Vec<(PathBuf, NewContainer)>,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("import path is not UTF-8"))?;
        if kind.is_dir() {
            collect(&entry.path(), &folder.child(&name)?, items)?;
        } else if kind.is_file() {
            items.push((
                entry.path(),
                NewContainer {
                    folder: folder.clone(),
                    name: FileName::parse(name)?,
                    tags: Vec::new(),
                },
            ));
        } else {
            return Err(invalid(format!(
                "unsupported import entry {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}
