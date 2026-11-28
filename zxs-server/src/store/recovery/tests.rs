use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use crate::{
    domain::{
        Blake3Hash, Change, Commit, Container, ContainerId, Detail, DiskName, FileName, FolderMove,
        FolderPath, METADATA_LIMIT, Pending, RecycleId, RecycleInfo, Recycled, Result, TxId,
        Version, VersionNum, tree_recycle,
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
        let transactions = store.pending_changes()?;
        assert_eq!(transactions.len(), 1);
        for transaction in &transactions {
            store.apply(transaction, None)?;
            store.apply(transaction, None)?;
        }
        store.clean_stages()?;
        let detail = store.snapshot(&store.paths.container(&commit.container))?;
        assert_eq!(detail.versions.len(), 1);
        assert_eq!(
            fs::read(Layout::payload(
                &store.paths.container(&commit.container),
                &commit.version
            ))?,
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
fn replay_prune_after_a_partial_removal() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let mut commit = sample()?;
    for number in 1..=4 {
        commit.version.number = VersionNum::new(number)?;
        commit.container.latest_version = commit.version.number;
        stage_publish(&store, &commit, false)?;
        for transaction in store.pending_changes()? {
            store.apply(&transaction, None)?;
            store.end(&transaction)?;
        }
        store.clean_stages()?;
    }
    let path = store.paths.container(&commit.container);
    let id = commit.container.id.clone();
    let pending = job(
        Change::Prune {
            container: commit.container,
            first_retained_version: VersionNum::new(4)?,
        },
        "c",
    )?;
    let _metadata = store.prepare_metadata(&pending)?;
    store.start(&pending)?;
    fs::remove_dir_all(Layout::version(&path, VersionNum::FIRST))?;
    assert!(store.snapshot(&path).is_err());
    let blocked = Layout::version(&path, VersionNum::new(3)?);
    let saved = root.0.join("saved-version");
    fs::rename(&blocked, &saved)?;
    fs::write(&blocked, b"expected a directory")?;
    assert!(matches!(
        store.apply(&pending, None),
        Err(crate::domain::Error::Io(_))
    ));
    assert_eq!(
        super::require_container(&path, &id)?.first_retained_version,
        VersionNum::FIRST
    );
    let transactions = store.pending_changes()?;
    assert_eq!(transactions.len(), 1);
    fs::remove_file(&blocked)?;
    fs::rename(&saved, &blocked)?;
    store.apply(&transactions[0], None)?;
    store.apply(&transactions[0], None)?;
    assert_eq!(store.snapshot(&path)?.versions, vec![commit.version]);
    Ok(())
}

#[test]
fn recycle_and_restore_are_idempotent() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let commit = sample()?;
    stage_publish(&store, &commit, false)?;
    for transaction in store.pending_changes()? {
        store.apply(&transaction, None)?;
        store.end(&transaction)?;
    }
    store.clean_stages()?;
    let entry = Recycled {
        recycle: RecycleInfo {
            recycle_id: RecycleId::parse("c".repeat(32))?,
            recycled_at: 2,
        },
        recycle_disk: DiskName::parse("rec1")?,
        container: (&commit.container).into(),
    };
    let recycled_path = store.paths.recycled_entry(&entry);
    let recycle = job(
        Change::Recycle {
            source: commit.container.folder.clone(),
            entry: entry.clone(),
        },
        "d",
    )?;
    let _stage = store.stage_recycle(&entry.recycle_disk, &recycle.tx_id)?;
    store.prepare_archive(
        &recycle.tx_id,
        &entry,
        &Detail {
            container: commit.container.clone(),
            versions: vec![commit.version.clone()],
        },
    )?;
    let incoming = store.paths.incoming_entry(
        &entry.recycle_disk,
        &recycle.tx_id,
        &entry.recycle.recycle_id,
    );
    assert!(incoming.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    let payload = Layout::payload(&store.paths.container(&commit.container), &commit.version);
    assert!(payload.is_file());
    store.apply(&recycle, None)?;
    store.apply(&recycle, None)?;
    assert!(!store.paths.container(&commit.container).exists());
    assert!(recycled_path.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    let restore = job(
        Change::Restore {
            container: commit.container.clone(),
            entry: entry.clone(),
        },
        "e",
    )?;
    let disk = commit.container.folder.disk()?;
    let (_restore_stage, versions) = store.prepare_restore(&entry, &disk, &restore.tx_id)?;
    assert_eq!(versions, vec![commit.version.clone()]);
    assert!(recycled_path.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    assert!(
        store
            .paths
            .staged_tree(&disk, &restore.tx_id)
            .join("v000001/PHOTO.JPG")
            .is_file()
    );
    let _metadata = store.prepare_metadata(&restore)?;
    store.apply(&restore, None)?;
    let mut online = store.clone();
    online.paths.recycle_disks.clear();
    assert_eq!(online.apply(&restore, None)?, Vec::<PathBuf>::new());
    assert!(!recycled_path.exists());
    assert_eq!(fs::read(payload)?, b"photo");
    Ok(())
}

#[test]
fn large_recycle_tree_plan_records_all_archives_in_one_file() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let pending = job(
        Change::RecycleTree {
            folder: FolderPath::parse("/disk1/project")?,
        },
        "d",
    )?;
    let disk = DiskName::parse("rec1")?;
    let template = sample()?.container;
    let entries = (0..8192)
        .map(|index| {
            let mut container = template.clone();
            container.id = ContainerId::parse(format!("{index:032x}"))?;
            tree_recycle(&pending.tx_id, 2, &container, disk.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let stage = store.stage_plan(&pending.tx_id, &entries)?;
    assert_eq!(fs::read_dir(&stage.path)?.count(), 1);
    assert!(
        fs::metadata(stage.path.join("plan.json"))?.len() > crate::domain::METADATA_LIMIT as u64
    );
    let recovered = store.recover_archives(&pending)?;
    assert_eq!(recovered.len(), entries.len());
    for (actual, expected) in recovered.iter().zip(&entries) {
        assert_eq!(actual.recycle.recycle_id, expected.recycle.recycle_id);
    }
    Ok(())
}

#[test]
fn replay_recycle_tree_after_archive_publication() -> Result<()> {
    let root = TestDir::create()?;
    let mut store = root.store()?;
    let mut commit = sample()?;
    let folder = FolderPath::parse("/disk1/项目")?;
    commit.container.folder = folder.clone();
    let mut second = commit.clone();
    second.container.id = ContainerId::parse("c".repeat(32))?;
    second.container.name = FileName::parse("SECOND.JPG")?;
    for original in [&commit, &second] {
        stage_publish(&store, original, false)?;
        for transaction in store.pending_changes()? {
            store.apply(&transaction, None)?;
            store.end(&transaction)?;
        }
        store.clean_stages()?;
    }

    let pending = job(
        Change::RecycleTree {
            folder: folder.clone(),
        },
        "d",
    )?;
    let disk = DiskName::parse("rec1")?;
    let _stage = store.stage_recycle(&disk, &pending.tx_id)?;
    let mut entries = Vec::new();
    for original in [&commit, &second] {
        let detail = store.snapshot(&store.paths.container(&original.container))?;
        let entry = tree_recycle(&pending.tx_id, 2, &detail.container, disk.clone())?;
        store.prepare_archive(&pending.tx_id, &entry, &detail)?;
        entries.push(entry);
    }
    let _plan = store.stage_plan(&pending.tx_id, &entries)?;
    store.start(&pending)?;
    let entry = &entries[0];
    let batch = store.paths.recycled_entry(entry);
    let incoming = store.paths.incoming_entry(
        &entry.recycle_disk,
        &pending.tx_id,
        &entry.recycle.recycle_id,
    );
    let detached = store.detach_source(
        &store.paths.folder(&folder),
        &folder.disk()?,
        &pending.tx_id,
    )?;
    // The process could stop after any archive rename, before either parent was synced.
    fs::rename(&incoming, &batch)?;
    store.paths.data_disks.clear();

    assert_eq!(store.apply(&pending, None)?, Vec::<PathBuf>::new());
    assert_eq!(store.apply(&pending, None)?, Vec::<PathBuf>::new());
    assert!(!store.paths.folder(&folder).exists());
    assert_eq!(
        fs::read(batch.join("PHOTO.JPG/v000001/PHOTO.JPG"))?,
        b"photo"
    );
    assert_eq!(
        fs::read(
            store
                .paths
                .recycled_entry(&entries[1])
                .join("SECOND.JPG/v000001/PHOTO.JPG")
        )?,
        b"photo"
    );
    assert!(detached.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    Ok(())
}

#[test]
fn replay_clear_recycle_after_the_last_batch_was_renamed() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let disk = DiskName::parse("rec1")?;
    let pending = job(
        Change::PurgeAll {
            disks: vec![disk.clone()],
        },
        "e",
    )?;
    let trash = store.paths.trash(&disk, &pending.tx_id);
    mkdir(&trash.join("already-renamed"))?;
    store.start(&pending)?;

    // Replay must sync the trash and recycle roots even when there are no batches left.
    let cleanup = store.apply(&pending, None)?;
    assert_eq!(cleanup, std::slice::from_ref(&trash));
    store.end(&pending)?;
    Store::finish(cleanup);
    assert!(!trash.exists());
    Ok(())
}

#[test]
fn replay_same_disk_container_move_preserves_the_original_name() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let mut commit = sample()?;
    commit.container.folder = FolderPath::parse("/disk1/项目/资料")?;
    let tag = "a".repeat(METADATA_LIMIT * 3 / 4);
    commit.container.tags = vec![tag.clone()];
    stage_publish(&store, &commit, false)?;
    for transaction in store.pending_changes()? {
        store.apply(&transaction, None)?;
        store.end(&transaction)?;
    }
    let source = store.paths.container(&commit.container);
    assert_eq!(fs::read(source.join("v000001/PHOTO.JPG"))?, b"photo");

    let mut updated = commit.container.clone();
    updated.folder = FolderPath::parse("/disk1/归档")?;
    updated.name = FileName::parse("图片.jpg")?;
    let target = store.paths.container(&updated);
    mkdir(parent(&target)?)?;
    let patch = job(
        Change::Patch {
            previous: commit.container,
            container: updated,
            staged: false,
        },
        "f",
    )?;
    let mut metadata = store.prepare_metadata(&patch)?;
    store.start(&patch)?;
    for stage in metadata.as_mut_slice() {
        stage.journaled();
    }
    assert!(fs::metadata(store.journal(&patch))?.len() > METADATA_LIMIT as u64);
    fs::rename(&source, &target)?;
    let journals = store.pending_changes()?;
    assert_eq!(journals.len(), 1);
    for pending in &journals {
        store.apply(pending, None)?;
        store.apply(pending, None)?;
        store.end(pending)?;
    }
    assert!(store.paths.data.join("disk1/项目/资料").is_dir());
    assert_eq!(fs::read(target.join("v000001/PHOTO.JPG"))?, b"photo");
    let detail = store.snapshot(&target)?;
    assert_eq!(detail.container.folder.as_str(), "/disk1/归档");
    assert_eq!(detail.container.name.as_str(), "图片.jpg");
    assert_eq!(detail.container.tags, [tag]);
    assert_eq!(store.live_tree()?.containers, vec![target]);
    Ok(())
}

#[test]
fn replay_cross_disk_move_after_source_detach_and_after_publication() -> Result<()> {
    let root = TestDir::create()?;
    let mut store = root.store()?;
    crate::test_support::prepare_disk(&store.paths.data.join("disk2"), "data", 3)?;
    store.paths.data_disks = store.register_disks(DiskKind::Data, &store.paths.data_disks)?;
    store.prepare()?;

    let from = FolderPath::parse("/disk1/项目")?;
    let to = FolderPath::parse("/disk2/归档")?;
    let mut commit = sample()?;
    commit.container.folder = from.clone();
    stage_publish(&store, &commit, false)?;
    for transaction in store.pending_changes()? {
        store.apply(&transaction, None)?;
        store.end(&transaction)?;
    }
    mkdir(&store.paths.folder(&from).join("空目录"))?;
    let metadata = fs::read(Layout::metadata(&store.paths.container(&commit.container)))?;
    let pending = job(
        Change::MoveFolder {
            from: from.clone(),
            to: to.clone(),
            staged: true,
        },
        "d",
    )?;
    let mut stage = store
        .prepare_move(
            &FolderMove {
                from: from.clone(),
                to: to.clone(),
            },
            &pending.tx_id,
            |copy| Store::inspect_tree(copy, |path| Store::verify_versions(path).map(|_| ())),
        )?
        .ok_or(crate::domain::Error::Recovery)?;
    store.start(&pending)?;
    stage.journaled();
    let detached =
        store.detach_source(&store.paths.folder(&from), &from.disk()?, &pending.tx_id)?;
    assert_eq!(
        store.apply(&pending, None)?,
        std::slice::from_ref(&detached)
    );
    store
        .paths
        .data_disks
        .retain(|disk| disk.name.as_str() == "disk2");

    let pending = store
        .pending_changes()?
        .pop()
        .ok_or(crate::domain::Error::Recovery)?;
    assert_eq!(store.apply(&pending, None)?, Vec::<PathBuf>::new());
    store.end(&pending)?;
    store.clean_stages()?;
    assert!(!store.paths.folder(&from).exists());
    assert!(store.paths.folder(&to).join("空目录").is_dir());
    assert!(detached.join("PHOTO.JPG/v000001/PHOTO.JPG").is_file());
    commit.container.folder = to;
    let path = store.paths.container(&commit.container);
    assert_eq!(fs::read(Layout::metadata(&path))?, metadata);
    assert_eq!(fs::read(Layout::payload(&path, &commit.version))?, b"photo");
    Ok(())
}

fn job(change: Change, digit: &str) -> Result<Pending> {
    Ok(Pending {
        tx_id: TxId::parse(digit.repeat(32))?,
        change,
    })
}
