use std::{sync::mpsc, thread, time::Duration};

use crate::{
    domain::{Error, Result},
    test_support::TestDir,
};

use super::Db;

#[test]
fn sqlite_waits_for_locks_and_readers_skip_a_busy_slot() -> Result<()> {
    let directory = TestDir::create()?;
    let (db, missing) = Db::open(&directory.0.join("index.sqlite"))?;
    assert!(missing);
    db.rebuild(None, |_| Ok(()))?;
    let held = db.readers[0].lock().map_err(|_| Error::Recovery)?;
    let timeout = thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        let db = &db;
        scope.spawn(move || {
            let result: Result<u64> = db.read().and_then(|reader| {
                Ok(reader.pragma_query_value(None, "busy_timeout", |row| row.get(0))?)
            });
            let _ = sender.send(result);
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        drop(held);
        result
    });
    assert_eq!(timeout.map_err(|_| Error::Recovery)??, 5_000);
    Ok(())
}
