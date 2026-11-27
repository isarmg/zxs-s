use std::{sync::mpsc, thread, time::Duration};

use rusqlite::{Connection, StatementStatus, params};

use crate::{
    domain::{Error, Result},
    test_support::TestDir,
};

use super::{Db, LIST_CONTAINERS, SCHEMA};

#[test]
fn late_container_pages_seek_to_the_cursor() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    connection.execute_batch(
        "WITH RECURSIVE entries(i) AS (
            VALUES(0) UNION ALL SELECT i+1 FROM entries WHERE i<19999
         ), keys(folder,name) AS (
            SELECT CASE WHEN i<10000 THEN '/disk' ELSE printf('/disk/%03d',(i-10000)/1000) END,
                   printf('%06d',i) FROM entries
         )
         INSERT INTO containers SELECT name,folder,folder,name,name,folder||'/'||name,'','{}' FROM keys",
    )?;
    let mut listing = connection.prepare(LIST_CONTAINERS)?;
    for (folder, first, end, cursor_folder, cursor_name) in [
        ("/disk", "/disk", "/disk", "/disk", "009980"),
        ("/disk", "/disk/", "/disk0", "/disk/009", "019980"),
        ("/", "/", "0", "/disk/009", "019980"),
    ] {
        let names = listing
            .query_map(
                params![
                    cursor_folder,
                    cursor_name,
                    cursor_name,
                    folder,
                    None::<String>,
                    None::<String>,
                    11,
                    first,
                    end
                ],
                |row| row.get::<_, String>(1),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(names.len(), 11);
        let steps = listing.reset_status(StatementStatus::VmStep);
        assert!(
            steps < 1_000,
            "listing {folder} after {cursor_name} took {steps} SQLite instructions"
        );
    }
    Ok(())
}

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
