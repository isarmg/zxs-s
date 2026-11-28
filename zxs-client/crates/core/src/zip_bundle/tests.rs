use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    domain::{EntryKind, Error, LocalPath, Member, Result, Snapshot, invalid},
    store::{
        digest,
        layout::{Layout, TempDir, parent, publish_staged_file, same_existing_path},
    },
};

use super::{
    Bundle, BundleInstall, apply_bundle, collect, manifest_path, pack, plan_bundle,
    rules::collect_backup,
};

#[test]
fn bundle_is_deterministic_and_honors_only_the_manifest() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let root = stage.child("source");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("ignored"))?;
    let rules = b"ignored/\n*.tmp\n!keep.tmp\n";
    fs::write(root.join(".zxsignore"), rules)?;
    fs::write(root.join(".gitignore"), "visible.txt\n")?;
    fs::write(root.join("ignored/one.txt"), "ignore this")?;
    fs::write(root.join("skip.tmp"), "ignore this too")?;
    fs::write(root.join("keep.tmp"), "include")?;
    fs::write(root.join("visible.txt"), "include")?;
    let members = required_members(&root)?;
    let names: Vec<_> = members.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(
        names,
        [".gitignore", ".zxsignore", "keep.tmp", "visible.txt"]
    );
    let first = stage.child("first.zip");
    let second = stage.child("second.zip");
    pack(&root, &members, &first)?;
    pack(&root, &members, &second)?;
    assert_eq!(fs::read(&first)?, fs::read(second)?);
    let mut archive = ZipArchive::new(File::open(first)?)?;
    let mut manifest = archive.by_index(0)?;
    assert_eq!(manifest.name(), ".zxsignore");
    assert_eq!(manifest.header_start(), 0);
    let mut bytes = Vec::new();
    manifest.read_to_end(&mut bytes)?;
    assert_eq!(bytes, rules);
    Ok(())
}

#[test]
fn pack_requires_a_regular_manifest_with_the_canonical_name() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let root = stage.child("source");
    fs::create_dir(&root)?;
    fs::write(root.join(".zxsignore"), b"private.local\n")?;
    fs::write(root.join("public.txt"), b"article")?;
    let mut members = required_members(&root)?;
    let mut manifest = members.remove(0);
    let mut wrong_case = manifest.clone();
    wrong_case.path = LocalPath::parse(".ZXSIGNORE")?;
    manifest.kind = EntryKind::Directory;
    manifest.stamp = None;

    for (archive, entry) in [
        ("missing.zip", None),
        ("wrong-case.zip", Some(wrong_case)),
        ("directory.zip", Some(manifest)),
    ] {
        let mut entries = members.clone();
        entries.extend(entry);
        assert!(matches!(
            pack(&root, &entries, &stage.child(archive)),
            Err(Error::Invalid(message)) if message.contains("regular root file named .zxsignore")
        ));
    }
    assert_eq!(fs::read(root.join(".zxsignore"))?, b"private.local\n");
    assert_eq!(fs::read(root.join("public.txt"))?, b"article");
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
    assert!(manifest_path(&root)?.is_none());
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
    let previous = Snapshot::Bundle(required_members(&local)?);
    fs::write(local.join(".zxsignore"), b"*.local\n")?;
    fs::write(local.join("notes.local"), b"local edit to preserve")?;

    fs::write(remote.join(".zxsignore"), b"notes.local\n")?;
    fs::write(remote.join("new.local"), b"newly included member")?;
    fs::write(remote.join("keep.txt"), b"new version")?;
    let archive = stage.child("incoming.zip");
    pack(&remote, &required_members(&remote)?, &archive)?;
    let mut bundle = Bundle::open(&archive)?.ok_or_else(|| invalid("expected a test bundle"))?;

    let expanded = stage.child("expanded");
    let plan = plan_bundle(&local, &expanded, &mut bundle, Some(&previous), false)?;
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
    assert_eq!(fs::read(local.join("new.local"))?, b"newly included member");
    assert!(!local.join("remove.txt").try_exists()?);
    Ok(())
}

#[test]
fn case_only_member_renames_distinguish_aliases_from_separate_entries() -> Result<()> {
    let stage = TempDir::create(&std::env::temp_dir())?;
    let local = stage.child("local");
    let remote = stage.child("remote");
    fs::create_dir_all(local.join("config/nested"))?;
    fs::create_dir_all(remote.join("Config/nested"))?;
    fs::write(local.join(".zxsignore"), b"")?;
    fs::write(remote.join(".zxsignore"), b"")?;
    let changes = [
        ("config/nested/settings.txt", "Config/nested/settings.txt"),
        ("Note.txt", "note.txt"),
    ];
    for (old, new) in changes {
        fs::write(local.join(old), b"old contents")?;
        fs::write(remote.join(new), b"incoming contents")?;
    }
    let previous = Snapshot::Bundle(required_members(&local)?);
    let archive = stage.child("incoming.zip");
    pack(&remote, &required_members(&remote)?, &archive)?;
    let mut bundle = Bundle::open(&archive)?.ok_or_else(|| invalid("expected a test bundle"))?;

    for (index, (old, new)) in changes.iter().enumerate() {
        let source = local.join(old);
        let target = local.join(new);
        if target.try_exists()? {
            assert!(same_existing_path(&source, &target));
            continue;
        }
        fs::create_dir_all(parent(&target)?)?;
        fs::write(&target, b"separate local contents")?;
        assert!(!same_existing_path(&source, &target));
        let rejected = stage.child(&format!("rejected-{index}"));
        assert!(matches!(
            plan_bundle(&local, &rejected, &mut bundle, Some(&previous), true),
            Err(Error::LocalConflict(_))
        ));
        assert_eq!(fs::read(source)?, b"old contents");
        assert_eq!(fs::read(&target)?, b"separate local contents");
        fs::remove_file(target)?;
        if new.starts_with("Config/") {
            fs::remove_dir_all(local.join("Config"))?;
        }
    }

    let expanded = stage.child("expanded");
    let plan = plan_bundle(&local, &expanded, &mut bundle, Some(&previous), false)?;
    apply_bundle(
        &local,
        &plan,
        BundleInstall {
            stage: &expanded,
            slot: &stage.id()?,
            backup: Some(&stage.child("backup")),
        },
    )?;
    let members = required_members(&local)?;
    for (old, new) in changes {
        assert!(!members.iter().any(|member| member.path.as_str() == old));
        assert!(members.iter().any(|member| member.path.as_str() == new));
        assert_eq!(fs::read(local.join(new))?, b"incoming contents");
    }
    Ok(())
}

fn required_members(root: &Path) -> Result<Vec<Member>> {
    let manifest = manifest_path(root)?.ok_or_else(|| invalid("test bundle missing manifest"))?;
    collect(root, &manifest)
}
