use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use rayon::prelude::*;

use crate::{
    domain::{Detail, DiskName, Error, Recycled, Result},
    store::{
        Layout, Store,
        db::{self, Db},
    },
};

enum Record {
    Container(Detail),
    Recycled(Recycled),
    Scanned(Result<()>),
}

pub(in crate::service) fn rebuild(store: &Store, db: &Db) -> Result<()> {
    let live = store.live_tree()?;
    let archives = store.recycle_paths(true)?;
    let (send, receive) = mpsc::sync_channel(16);
    thread::scope(|scope| {
        let containers = live.containers.as_slice();
        let archives = archives.as_slice();
        let worker = scope.spawn(move || scan(store, containers, archives, &send));
        let indexed = db.rebuild(|tx| {
            for folder in &live.folders {
                db::put_folder(tx, folder)?;
            }
            index_records(tx, &receive)
        });
        drop(receive);
        worker.join().map_err(|_| Error::Recovery)?;
        indexed
    })
}

fn index_records(tx: &rusqlite::Transaction<'_>, input: &Receiver<Record>) -> Result<()> {
    loop {
        match input.recv().map_err(|_| Error::Recovery)? {
            Record::Container(detail) => db::put_detail(tx, &detail)?,
            Record::Recycled(entry) => db::put_recycle(tx, &entry)?,
            Record::Scanned(result) => return result,
        }
    }
}

fn scan(
    store: &Store,
    containers: &[PathBuf],
    archives: &[(DiskName, PathBuf)],
    send: &SyncSender<Record>,
) {
    let result = containers
        .par_iter()
        .try_for_each(|path| push(send, Record::Container(store.snapshot(path)?)))
        .and_then(|()| {
            archives.par_iter().try_for_each(|(disk, path)| {
                let mut entry = store.archive_entry(disk, path)?;
                let container = Layout::recycled_container(path, &entry.container.name);
                entry.container = Store::versions_at(&container, entry.container)?.container;
                push(send, Record::Recycled(entry))
            })
        });
    let _ = send.send(Record::Scanned(result));
}

fn push(queue: &SyncSender<Record>, item: Record) -> Result<()> {
    queue.send(item).map_err(|_| Error::Recovery)
}
