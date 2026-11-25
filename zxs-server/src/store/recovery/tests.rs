use std::fs;

use crate::{
    domain::{
        Blake3Hash, Change, Commit, Container, ContainerId, FileName, FolderPath, OpId, Pending,
        RecycleId, RecycleInfo, Recycled, Result, Version, VersionNum,
    },
    test_support::TestDir,
};

use super::{Layout, Store, rename};

fn store(root: &TestDir) -> Result<Store> {
    let store = Store {
        paths: Layout {
            data: root.0.join("data"),
            recycle: root.0.join("recycle"),
        },
    };
    store.prepare()?;
    Ok(store)
}

fn sample() -> Result<Commit> {
    let name = FileName::parse("PHOTO.JPG")?;
    Ok(Commit {
        container: Container {
            id: ContainerId::parse("a".repeat(32))?,
            folder: FolderPath::parse("/")?,
            name: name.clone(),
            tags: Vec::new(),
            latest_version: VersionNum::FIRST,
            created_at: 1,
            updated_at: 1,
        },
        version: Version {
            number: VersionNum::FIRST,
            op_id: OpId::parse("b".repeat(32))?,
            created_at: 1,
            name_at_commit: name,
            size: 5,
            blake3: Blake3Hash::parse(blake3::hash(b"photo").to_hex().to_string())?,
            payload_file: FileName::parse("payload.bin")?,
        },
    })
}

fn stage_publish(store: &Store, commit: &Commit, renamed: bool) -> Result<()> {
    let version = &commit.version;
    let id = &commit.container.id;
    let mut stage = store.stage(id, &version.op_id)?;
    fs::write(stage.payload(&version.payload_file), b"photo")?;
    stage.save_version(version)?;
    store.start(&Pending {
        change: Change::Publish {
            commit: Box::new(commit.clone()),
        },
        record: None,
    })?;
    stage.journaled();
    if renamed {
        rename(
            &stage.path,
            &Layout::version(&store.paths.container(id), version.number),
        )?;
    }
    Ok(())
}

#[test]
fn replay_publish_before_and_after_payload_rename() -> Result<()> {
    for renamed in [false, true] {
        let root = TestDir::create()?;
        let store = store(&root)?;
        let commit = sample()?;
        let id = &commit.container.id;
        stage_publish(&store, &commit, renamed)?;
        let transactions = store.recover_changes()?;
        assert_eq!(transactions.len(), 1);
        for transaction in &transactions {
            store.apply(&transaction.change)?;
            store.apply(&transaction.change)?;
        }
        store.clean_stages()?;
        let detail = Store::snapshot(&store.paths.container(id))?;
        assert_eq!(detail.versions.len(), 1);
        assert_eq!(
            fs::read(store.paths.payload(id, &commit.version))?,
            b"photo"
        );
        assert!(!store.paths.stage_marker(&commit.version.op_id).exists());
    }
    Ok(())
}

#[test]
fn recycle_and_restore_are_idempotent() -> Result<()> {
    let root = TestDir::create()?;
    let store = store(&root)?;
    let commit = sample()?;
    let id = &commit.container.id;
    stage_publish(&store, &commit, false)?;
    for transaction in store.recover_changes()? {
        store.apply(&transaction.change)?;
    }
    store.clean_stages()?;
    let recycle_id = RecycleId::parse("c".repeat(32))?;
    let entry = Recycled {
        recycle: RecycleInfo {
            recycle_id: recycle_id.clone(),
            op_id: OpId::parse("d".repeat(32))?,
            recycled_at: 2,
        },
        container: commit.container.clone(),
    };
    let recycle = Change::Recycle { entry };
    store.apply(&recycle)?;
    store.apply(&recycle)?;
    assert!(!store.paths.container(id).exists());
    assert!(store.paths.recycled_entry(&recycle_id).is_dir());
    let restore = Change::Restore {
        container: commit.container.clone(),
        recycle_id: recycle_id.clone(),
    };
    store.apply(&restore)?;
    store.apply(&restore)?;
    assert!(!store.paths.recycled_entry(&recycle_id).exists());
    assert_eq!(
        fs::read(store.paths.payload(id, &commit.version))?,
        b"photo"
    );
    Ok(())
}
