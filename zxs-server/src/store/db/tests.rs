use std::{sync::mpsc, thread, time::Duration};

use rusqlite::{Connection, StatementStatus, params};

use crate::{
    domain::{
        Blake3Hash, Change, Container, ContainerId, Error, FileName, FolderPath, Pending, Result,
        TxId, VersionNum,
    },
    test_support::TestDir,
};

use super::{
    Db, LIST_CONTAINERS, SCHEMA, SUBTREE_CONTAINERS, apply_change, container_listing,
    put_container_with_ancestors,
};

fn listing_fixture() -> Result<Connection> {
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
    Ok(connection)
}

#[test]
fn rename_does_not_rewrite_unchanged_tags() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    let tx = connection.unchecked_transaction()?;
    let previous = Container {
        id: ContainerId::parse("a".repeat(32))?,
        folder: FolderPath::parse("/disk/work")?,
        name: FileName::parse("old.txt")?,
        tags: vec!["first".into(), "second".into()],
        latest_version: VersionNum::FIRST,
        latest_blake3: Some(Blake3Hash::parse("a".repeat(64))?),
        created_at: 1,
        updated_at: 1,
    };
    put_container_with_ancestors(&tx, &previous)?;
    let changes = tx.total_changes();
    let container = Container {
        name: FileName::parse("new.txt")?,
        updated_at: 2,
        ..previous.clone()
    };
    let pending = Pending {
        tx_id: TxId::parse("b".repeat(32))?,
        change: Change::Patch {
            previous,
            container,
            staged: false,
        },
    };
    apply_change(&tx, &pending, None, &[])?;
    assert_eq!(tx.total_changes() - changes, 1);
    assert_eq!(
        tx.query_row("SELECT count(*) FROM tags", [], |r| r.get::<_, u64>(0))?,
        2
    );
    Ok(())
}

#[test]
fn late_container_pages_seek_to_the_cursor() -> Result<()> {
    let connection = listing_fixture()?;
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
fn subtree_inventory_keeps_order_without_sorting_or_including_neighbors() -> Result<()> {
    let connection = listing_fixture()?;
    for sibling in ["/disk-old", "/disk0"] {
        connection.execute(
            "INSERT INTO containers VALUES (?1,?1,?1,'other','other',?1||'/other','','{}')",
            [sibling],
        )?;
    }
    let mut listing = connection.prepare(SUBTREE_CONTAINERS)?;
    let names = listing
        .query_map(["/disk", "/disk/", "/disk0"], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let expected: Vec<_> = (0..20_000).map(|n| format!("{n:06}")).collect();
    assert_eq!(names, expected);
    assert_eq!(listing.get_status(StatementStatus::Sort), 0);
    Ok(())
}

#[test]
fn tagged_pages_use_the_tag_index_for_sparse_filters() -> Result<()> {
    let connection = listing_fixture()?;
    connection.execute_batch(
        "INSERT INTO tags SELECT id,'rare' FROM containers WHERE name >= '019980';
         INSERT INTO tags SELECT id,'common' FROM containers WHERE substr(name,6,1) < '5'",
    )?;
    for (tag, expected) in [("absent", 0), ("rare", 20), ("common", 101)] {
        let mut listing = connection.prepare(container_listing(&connection, Some(tag))?)?;
        let names = listing
            .query_map(
                params!["/", "", "", "/", tag, None::<String>, 101, "/", "0"],
                |row| row.get::<_, String>(1),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(names.len(), expected);
        let steps = listing.get_status(StatementStatus::VmStep);
        assert!(
            steps < 10_000,
            "listing tag {tag} took {steps} SQLite instructions"
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
