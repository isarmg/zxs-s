use std::{
    fs::{self, File},
    io::Write,
};

use zip::{ZipWriter, write::SimpleFileOptions};

use crate::{
    domain::{Result, Snapshot, invalid},
    store::{
        digest,
        layout::{Layout, TempDir, publish_staged_file},
    },
};

use super::{
    Bundle, BundleInstall, apply_bundle, collect, pack, plan_bundle, rules::collect_backup,
};

#[test]
fn bundle_is_deterministic_and_honors_only_the_manifest() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let root = stage.child("source");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("ignored"))?;
    fs::write(root.join(".zxsignore"), "ignored/\n*.tmp\n!keep.tmp\n")?;
    fs::write(root.join(".gitignore"), "visible.txt\n")?;
    fs::write(root.join("ignored/one.txt"), "ignore this")?;
    fs::write(root.join("skip.tmp"), "ignore this too")?;
    fs::write(root.join("keep.tmp"), "include")?;
    fs::write(root.join("visible.txt"), "include")?;
    let members = collect(&root)?;
    let names: Vec<_> = members.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(
        names,
        [".gitignore", ".zxsignore", "keep.tmp", "visible.txt"]
    );
    let first = stage.child("first.zip");
    let second = stage.child("second.zip");
    pack(&root, &members, &first)?;
    pack(&root, &members, &second)?;
    assert_eq!(fs::read(first)?, fs::read(second)?);
    Ok(())
}

#[test]
fn backup_without_manifest_skips_workspace_metadata() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let root = stage.child("source");
    fs::create_dir(&root)?;
    fs::create_dir(root.join(".zxs"))?;
    fs::write(root.join(".zxs/internal"), "metadata")?;
    fs::write(root.join("new.txt"), "edit to preserve")?;
    assert!(collect(&root).is_err());
    let members = collect_backup(&root)?;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].path.as_str(), "new.txt");
    Ok(())
}

#[test]
fn bundle_rejects_parent_traversal() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let path = stage.child("bundle.zip");
    let mut archive = ZipWriter::new(File::create(&path)?);
    archive.start_file(".zxsignore", SimpleFileOptions::default())?;
    archive.start_file("../outside.txt", SimpleFileOptions::default())?;
    archive.write_all(b"fixture")?;
    archive.finish()?;
    assert!(Bundle::open(&path).is_err());
    Ok(())
}

#[test]
fn file_publication_preserves_occupied_target_and_cleans_failed_staging() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let target = stage.child("current.txt");
    fs::write(&target, b"previous content")?;
    let payload = stage.child("incoming.txt");
    fs::write(&payload, b"incoming content")?;
    let hash = digest::file(&payload)?;
    let id = stage.id()?;
    let occupied = stage.child("directory");
    fs::create_dir(&occupied)?;
    assert!(publish_staged_file(&payload, &occupied, &id, &hash).is_err());
    assert!(!Layout::checkout_scratch(&occupied, &id).exists());
    fs::write(&payload, b"incoming content")?;
    assert!(publish_staged_file(&payload, &target, &id, &hash).is_err());
    assert_eq!(fs::read(&target)?, b"previous content");
    fs::remove_file(&target)?;
    fs::write(&payload, b"incoming content")?;
    publish_staged_file(&payload, &target, &id, &hash)?;
    assert_eq!(fs::read(&target)?, b"incoming content");
    Ok(())
}

#[test]
fn incremental_checkout_keeps_locally_ignored_files() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let local = stage.child("local");
    let remote = stage.child("source");
    fs::create_dir(&local)?;
    fs::create_dir(&remote)?;

    fs::write(local.join(".zxsignore"), b"")?;
    fs::write(local.join("notes.local"), b"previous tracked content")?;
    fs::write(local.join("remove.txt"), b"removed upstream")?;
    fs::write(local.join("keep.txt"), b"old version")?;
    let previous = Snapshot::Bundle(collect(&local)?);
    fs::write(local.join(".zxsignore"), b"*.local\n")?;
    fs::write(local.join("notes.local"), b"local edit to preserve")?;

    fs::write(remote.join(".zxsignore"), b"")?;
    fs::write(remote.join("notes.local"), b"incoming content")?;
    fs::write(remote.join("keep.txt"), b"new version")?;
    let archive = stage.child("incoming.zip");
    pack(&remote, &collect(&remote)?, &archive)?;
    let mut bundle = Bundle::open(&archive)?.ok_or_else(|| invalid("expected a test bundle"))?;

    let expanded = stage.child("expanded");
    let plan = plan_bundle(
        &local,
        &expanded,
        &mut bundle,
        Some(&previous),
        false,
        false,
    )?;
    apply_bundle(
        &local,
        &plan,
        BundleInstall {
            stage: &expanded,
            slot: &stage.id()?,
            backup: Some(&stage.child("backup")),
        },
    )?;
    assert_eq!(
        fs::read(local.join("notes.local"))?,
        b"local edit to preserve"
    );
    assert_eq!(fs::read(local.join("keep.txt"))?, b"new version");
    assert!(!local.join("remove.txt").try_exists()?);
    Ok(())
}
