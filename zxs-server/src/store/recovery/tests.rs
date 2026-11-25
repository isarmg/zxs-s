use std::{fs, os::unix::fs::PermissionsExt};

use crate::{
    domain::{
        Blake3Hash, Change, Commit, Container, ContainerId, DiskName, FileName, FolderPath,
        Pending, RecycleId, RecycleInfo, RecyclePlan, Recycled, Result, TxId, Version, VersionNum,
    },
    store::DiskKind,
    test_support::TestDir,
};

use super::{Layout, Store, mkdir, parent, rename};

fn sample() -> Result<Commit> {
    let name = FileName::parse("PHOTO.JPG")?;
    Ok(Commit {
        container: Container {
            id: ContainerId::parse("a".repeat(32))?,
            folder: FolderPath::parse("/disk1")?,
            name: name.clone(),
            tags: Vec::new(),
            latest_version: VersionNum::FIRST,
            latest_blake3: None,
            created_at: 1,
            updated_at: 1,
        },
        version: Version {
            number: VersionNum::FIRST,
            created_at: 1,
            name_at_commit: name,
            size: 5,
            blake3: Blake3Hash::parse(blake3::hash(b"photo").to_hex().to_string())?,
        },
    })
}

fn stage_publish(store: &Store, commit: &Commit, renamed: bool) -> Result<()> {
    let version = &commit.version;
    let tx_id = TxId::parse("b".repeat(32))?;
    let mut stage = store.stage_upload(&commit.container.folder.disk()?, &tx_id)?;
    let payload = stage.payload(&version.name_at_commit);
    fs::write(&payload, b"photo")?;
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o444))?;
    stage.save_version(version)?;
    let pending = Pending {
        tx_id,
        change: Change::Publish {
            commit: Box::new(commit.clone()),
        },
    };
    store.prepare_metadata(&pending)?;
    store.start(&pending)?;
    stage.journaled();
    if renamed {
        let destination =
            Layout::version(&store.paths.container(&commit.container), version.number);
        mkdir(parent(&destination)?)?;
        rename(&Layout::staged_version(&stage.path), &destination)?;
    }
    Ok(())
}

#[test]
fn replay_publish_before_and_after_payload_rename() -> Result<()> {
    for renamed in [false, true] {
        let root = TestDir::create()?;
        let store = root.store()?;
        let commit = sample()?;
        stage_publish(&store, &commit, renamed)?;
        let transactions = store.recover_changes()?;
        assert_eq!(transactions.len(), 1);
        for transaction in &transactions {
            store.apply(transaction)?;
            store.apply(transaction)?;
        }
        store.clean_stages()?;
        let detail = store.snapshot(&store.paths.container(&commit.container))?;
        assert_eq!(detail.versions.len(), 1);
        assert_eq!(
            fs::read(store.paths.payload(&commit.container, &commit.version))?,
            b"photo"
        );
        assert!(
            !store
                .paths
                .staging(
                    &commit.container.folder.disk()?,
                    &TxId::parse("b".repeat(32))?
                )
                .exists()
        );
    }
    Ok(())
}

#[test]
fn recycle_and_restore_are_idempotent() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let commit = sample()?;
    stage_publish(&store, &commit, false)?;
    for transaction in store.recover_changes()? {
        store.apply(&transaction)?;
    }
    store.clean_stages()?;
    let entry = Recycled {
        recycle: RecycleInfo {
            recycle_id: RecycleId::parse("c".repeat(32))?,
            recycled_at: 2,
        },
        recycle_disk: DiskName::parse("rec1")?,
        container: commit.container.clone(),
    };
    let recycled_path = store.paths.recycled_entry(&entry);
    let recycle = job(
        Change::Recycle {
            entry: entry.clone(),
            staged: false,
        },
        "d",
    )?;
    let _stage = store.stage_recycle(&entry.recycle_disk, &recycle.tx_id)?;
    store.prepare_archive(
        &recycle.tx_id,
        &RecyclePlan {
            entry: entry.clone(),
            staged: false,
        },
    )?;
    store.apply(&recycle)?;
    store.apply(&recycle)?;
    assert!(!store.paths.container(&commit.container).exists());
    assert!(recycled_path.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    let restore = job(
        Change::Restore {
            container: commit.container.clone(),
            entry,
            staged: false,
        },
        "e",
    )?;
    let _metadata = store.prepare_metadata(&restore)?;
    store.apply(&restore)?;
    store.apply(&restore)?;
    assert!(!recycled_path.exists());
    assert_eq!(
        fs::read(store.paths.payload(&commit.container, &commit.version))?,
        b"photo"
    );
    Ok(())
}

#[test]
fn replay_directory_move_preserves_the_original_name() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let mut commit = sample()?;
    commit.container.folder = FolderPath::parse("/disk1/项目/资料")?;
    stage_publish(&store, &commit, false)?;
    for transaction in store.recover_changes()? {
        store.apply(&transaction)?;
    }
    let source = store.paths.container(&commit.container);
    assert_eq!(fs::read(source.join("v000001/PHOTO.JPG"))?, b"photo");

    let mut updated = commit.container.clone();
    updated.folder = FolderPath::parse("/disk1/归档")?;
    updated.name = FileName::parse("图片.jpg")?;
    let target = store.paths.container(&updated);
    mkdir(parent(&target)?)?;
    rename(&source, &target)?;
    let patch = job(
        Change::Patch {
            previous: commit.container,
            container: updated,
            staged: false,
        },
        "f",
    )?;
    let _metadata = store.prepare_metadata(&patch)?;
    store.apply(&patch)?;
    store.apply(&patch)?;
    assert!(store.paths.data.join("disk1/项目/资料").is_dir());
    assert_eq!(fs::read(target.join("v000001/PHOTO.JPG"))?, b"photo");
    let detail = store.snapshot(&target)?;
    assert_eq!(detail.container.folder.as_str(), "/disk1/归档");
    assert_eq!(detail.container.name.as_str(), "图片.jpg");
    assert_eq!(store.live_tree()?.containers, vec![target]);
    Ok(())
}

#[test]
fn replay_cross_disk_move_after_target_is_published() -> Result<()> {
    let root = TestDir::create()?;
    let mut store = root.store()?;
    mkdir(&store.paths.data.join("disk2"))?;
    store.paths.data_disks = store.register_disks(DiskKind::Data, &store.paths.data_disks)?;
    store.prepare()?;

    let from = FolderPath::parse("/disk1/项目")?;
    let to = FolderPath::parse("/disk2/归档")?;
    let mut commit = sample()?;
    commit.container.folder = from.clone();
    stage_publish(&store, &commit, false)?;
    for transaction in store.recover_changes()? {
        store.apply(&transaction)?;
        store.end(&transaction)?;
    }
    mkdir(&store.paths.folder(&from).join("空目录"))?;
    let metadata = fs::read(Layout::container_meta(
        &store.paths.container(&commit.container),
    ))?;
    let pending = job(
        Change::MoveFolder {
            from: from.clone(),
            to: to.clone(),
            staged: true,
        },
        "d",
    )?;
    let mut stage = store
        .prepare_move(&from, &to, &pending.tx_id)?
        .ok_or(crate::domain::Error::Recovery)?;
    store.start(&pending)?;
    stage.journaled();
    rename(
        &store.paths.staged_tree(&to.disk()?, &pending.tx_id),
        &store.paths.folder(&to),
    )?;
    assert!(store.paths.folder(&from).exists());

    let pending = store
        .recover_changes()?
        .pop()
        .ok_or(crate::domain::Error::Recovery)?;
    store.apply(&pending)?;
    store.apply(&pending)?;
    store.end(&pending)?;
    store.finish(&pending)?;
    assert!(!store.paths.folder(&from).exists());
    assert!(store.paths.folder(&to).join("空目录").is_dir());
    commit.container.folder = to;
    let path = store.paths.container(&commit.container);
    assert_eq!(fs::read(Layout::container_meta(&path))?, metadata);
    assert_eq!(
        fs::read(store.paths.payload(&commit.container, &commit.version))?,
        b"photo"
    );
    Ok(())
}

fn job(change: Change, digit: &str) -> Result<Pending> {
    Ok(Pending {
        tx_id: TxId::parse(digit.repeat(32))?,
        change,
    })
}
