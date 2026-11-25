use std::fs;

use super::{
    FileName, Location, Move, Result, SETTINGS, TempDir, TxId, configured_workspace_in, copy_tree,
    move_workspace_in, parent, random_hex, rename_directory, save_metadata,
};
use crate::{Workspace, store::layout::WORKSPACE_DIR};

#[test]
fn a_move_persists_the_new_name_and_parent_without_recreating_the_index() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let settings = temp.child("settings");
    let original = temp.child("Original");
    let destination = temp.child("destination");
    fs::create_dir_all(original.join("disk1/reports"))?;
    fs::create_dir(&destination)?;
    Workspace::open(&original)?;
    fs::write(original.join("disk1/reports/draft.txt"), b"unsubmitted")?;
    let index = fs::read(original.join(".zxs/workspace.sqlite"))?;

    let renamed = move_workspace_in(
        &settings,
        &original,
        parent(&original)?,
        &FileName::parse("ORIGINAL")?,
    )?;
    assert_eq!(renamed.warnings.len(), 0, "{:?}", renamed.warnings);
    assert_eq!(
        renamed
            .path
            .canonicalize()?
            .file_name()
            .and_then(|name| name.to_str()),
        Some("ORIGINAL")
    );
    let moved = move_workspace_in(
        &settings,
        &renamed.path,
        &destination,
        &FileName::parse("New workspace")?,
    )?;
    assert_eq!(moved.warnings.len(), 0, "{:?}", moved.warnings);
    assert_eq!(
        moved.path,
        destination.canonicalize()?.join("New workspace")
    );
    assert_eq!(
        configured_workspace_in(&settings)?,
        Some(moved.path.clone())
    );
    assert!(!renamed.path.exists());
    assert_eq!(fs::read(moved.path.join(".zxs/workspace.sqlite"))?, index);
    assert_eq!(
        fs::read(moved.path.join("disk1/reports/draft.txt"))?,
        b"unsubmitted"
    );
    Ok(())
}

#[test]
fn a_move_rejects_an_existing_destination_and_a_descendant() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let settings = temp.child("settings");
    let original = temp.child("source");
    let occupied = temp.child("occupied");
    fs::create_dir_all(original.join("disk1"))?;
    fs::create_dir(&occupied)?;
    fs::write(original.join("disk1/draft"), b"keep")?;
    let name = FileName::parse("occupied")?;
    assert!(move_workspace_in(&settings, &original, parent(&occupied)?, &name).is_err());
    assert!(move_workspace_in(&settings, &original, &original.join("disk1"), &name).is_err());
    assert_eq!(fs::read(original.join("disk1/draft"))?, b"keep");
    assert!(occupied.is_dir());
    assert!(configured_workspace_in(&settings)?.is_none());
    Ok(())
}

#[test]
fn interrupted_moves_recover_a_staged_origin_or_published_destination() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let settings = temp.child("settings");
    let original = temp.child("source");
    let intermediate = temp.child("staging").join("workspace");
    fs::create_dir_all(original.join(WORKSPACE_DIR))?;
    fs::create_dir_all(parent(&intermediate)?)?;
    let original = original.canonicalize()?;
    let target = original.with_file_name("new-workspace");
    let movement = Move {
        from: original.clone(),
        to: target.clone(),
        intermediate: Some(intermediate.clone()),
        id: TxId::parse(random_hex::<16>()?)?,
    };
    fs::write(original.join("draft"), b"keep")?;
    movement.mark_origin()?;
    save_metadata(&settings, SETTINGS, &Location::Moving(movement.clone()))?;
    assert!(Workspace::open(&original).is_err());
    rename_directory(&original, &intermediate)?;
    assert_eq!(configured_workspace_in(&settings)?, Some(original.clone()));
    assert_eq!(fs::read(original.join("draft"))?, b"keep");
    Workspace::open(&original)?;

    movement.mark_origin()?;
    save_metadata(&settings, SETTINGS, &Location::Moving(movement))?;
    rename_directory(&original, &target)?;
    assert!(Workspace::open(&target).is_err());
    assert_eq!(configured_workspace_in(&settings)?, Some(target.clone()));
    assert_eq!(fs::read(target.join("draft"))?, b"keep");
    Workspace::open(&target)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn recovery_distinguishes_a_case_alias_from_a_finished_rename() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let settings = temp.child("settings");
    let original = temp.child("source");
    let target = temp.child("SOURCE");
    fs::create_dir_all(original.join(WORKSPACE_DIR))?;
    if !target.exists() {
        // Only a case-insensitive filesystem exposes the target as an alias.
        return Ok(());
    }
    let original = original.canonicalize()?;
    let movement = Move {
        from: original.clone(),
        to: original.with_file_name("SOURCE"),
        intermediate: Some(temp.child("staging/workspace")),
        id: TxId::parse(random_hex::<16>()?)?,
    };
    save_metadata(&settings, SETTINGS, &Location::Moving(movement.clone()))?;
    movement.mark_origin()?;
    assert_eq!(configured_workspace_in(&settings)?, Some(original.clone()));
    Workspace::open(&original)?;
    Ok(())
}

#[test]
fn a_cross_disk_copy_keeps_empty_dirs_and_file_timestamps() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let source = temp.child("source");
    let target = temp.child("target");
    fs::create_dir_all(source.join("disk1/empty"))?;
    let file = source.join("disk1/draft");
    fs::write(&file, b"unfinished draft")?;
    copy_tree::copy(&source, &target)?;
    copy_tree::verify(&source, &target)?;
    assert!(target.join("disk1/empty").is_dir());
    assert_eq!(
        fs::metadata(file)?.modified()?,
        fs::metadata(target.join("disk1/draft"))?.modified()?
    );
    fs::write(source.join("disk1/draft"), b"changed during the copy")?;
    assert!(copy_tree::verify(&source, &target).is_err());
    Ok(())
}
