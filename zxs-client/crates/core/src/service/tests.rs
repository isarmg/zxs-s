use std::{fs, path::Path};

use crate::{
    domain::{Blake3Hash, ContentHashes, InterruptedCheckout, MemberHashes},
    store::layout::{TempDir, stamp},
};

use super::{
    ContainerId, EntryKind, FileState, LocalPath, Result, Snapshot, Track, VersionNum, Workspace,
    browse::{ScanPurpose, WorkspaceEntry, WorkspacePage},
    invalid,
};

#[test]
fn workspace_listing_follows_the_current_directory() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    fs::create_dir_all(root.join("notes/nested"))?;
    fs::create_dir(root.join("empty"))?;
    fs::write(root.join("draft.txt"), b"draft")?;
    fs::write(root.join("notes/report.txt"), b"report")?;
    fs::write(root.join("notes/nested/todo.txt"), b"todo")?;
    fs::write(root.join(".zxsignore"), b"")?;

    let bundle = root.join("tracked");
    fs::create_dir(&bundle)?;
    fs::write(bundle.join(".zxsignore"), b"")?;
    fs::write(bundle.join("member.txt"), b"bundle member")?;

    let workspace = Workspace::open(&root)?;
    workspace.db.save(&fixture_track(
        "tracked",
        Snapshot::Bundle(crate::zip_bundle::collect(&bundle)?),
    )?)?;

    assert_eq!(workspace.status()?.entries.len(), 4);
    let entries = change_rows(&workspace, ".")?;
    let paths: Vec<_> = entries
        .iter()
        .map(|entry| entry.status.path.as_str())
        .collect();
    assert_eq!(paths, ["empty", "notes", "tracked", "draft.txt"]);
    assert_eq!(entries[0].local_kind, Some(EntryKind::Directory));
    assert!(!entries[0].can_submit);
    assert_eq!(entries[2].local_kind, Some(EntryKind::Directory));
    assert!(entries[2].status.bundle && !entries[2].can_submit);
    assert_eq!(entries[3].local_kind, Some(EntryKind::File));
    assert!(entries[1].can_submit && entries[3].can_submit);
    assert!(change_rows(&workspace, "empty")?.is_empty());

    let notes = change_rows(&workspace, "notes")?;
    let paths: Vec<_> = notes
        .iter()
        .map(|entry| entry.status.path.as_str())
        .collect();
    assert_eq!(paths, ["notes/nested", "notes/report.txt"]);
    assert_eq!(
        change_rows(&workspace, "notes/nested")?[0]
            .status
            .path
            .as_str(),
        "notes/nested/todo.txt"
    );

    let WorkspacePage::Bundle(tracked) = workspace.browse(Path::new("tracked"))? else {
        return Err(invalid("expected contents of the clean tracked bundle"));
    };
    let paths: Vec<_> = tracked
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["tracked/.zxsignore", "tracked/member.txt"]);
    Ok(())
}

#[test]
fn workspace_listing_keeps_missing_tracked_paths_reachable() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    fs::create_dir_all(root.join("removed"))?;
    let path = root.join("removed/draft.txt");
    fs::write(&path, b"draft")?;
    let workspace = Workspace::open(&root)?;
    workspace.db.save(&fixture_track(
        "removed/draft.txt",
        Snapshot::File(stamp(&path)?),
    )?)?;
    fs::remove_dir_all(root.join("removed"))?;

    let root_entries = change_rows(&workspace, ".")?;
    assert_eq!(root_entries.len(), 1);
    assert_eq!(root_entries[0].status.path.as_str(), "removed");
    assert_eq!(root_entries[0].local_kind, Some(EntryKind::Directory));
    assert!(!root_entries[0].can_submit);
    let missing = change_rows(&workspace, "removed")?;
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].status.path.as_str(), "removed/draft.txt");
    assert!(matches!(missing[0].status.state, FileState::Missing));
    assert!(missing[0].can_submit);
    Ok(())
}

#[test]
fn directory_submission_scan_covers_the_selected_tree_beyond_the_status_limit() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let directory = root.join("disk0/selected");
    fs::create_dir_all(directory.join("nested/empty"))?;
    fs::create_dir(directory.join("bundle"))?;
    fs::write(directory.join("bundle/.zxsignore"), b"")?;
    fs::write(directory.join("bundle/member.txt"), b"bundle member")?;
    fs::write(root.join("disk0/outside.txt"), b"outside scope")?;
    for index in 0..1001 {
        fs::write(directory.join(format!("nested/{index:04}.txt")), [])?;
    }

    let workspace = Workspace::open(&root)?;
    assert!(workspace.status()?.truncated);
    let scan =
        workspace.scan_directory(&LocalPath::parse("disk0/selected")?, ScanPurpose::Commit)?;
    let status = scan.status;
    assert!(!status.truncated);
    assert_eq!(status.entries.len(), 1002);
    assert!(status.entries.iter().all(|entry| {
        entry.path.as_str() == "disk0/selected/bundle"
            || entry.path.as_str().starts_with("disk0/selected/nested/")
    }));
    assert!(status.entries.iter().any(|entry| entry.bundle));
    assert_eq!(
        scan.empty_directories,
        [LocalPath::parse("disk0/selected/nested/empty")?]
    );
    Ok(())
}

#[test]
fn bundle_navigation_lists_local_members_without_creating_changes() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let bundle = root.join("disk0/project");
    fs::create_dir_all(bundle.join("nested/empty"))?;
    fs::write(bundle.join(".zxsignore"), b"*.local\n")?;
    fs::write(bundle.join("nested/report.txt"), b"report")?;
    fs::write(bundle.join("nested/private.local"), b"ignored local file")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("report.txt", bundle.join("nested/link.txt"))?;

    let workspace = Workspace::open(&root)?;
    let status = workspace.status()?;
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].path.as_str(), "disk0/project");
    assert!(status.entries[0].bundle);

    let WorkspacePage::Bundle(contents) = workspace.browse(&bundle.join("nested"))? else {
        return Err(invalid("expected contents of the nested bundle directory"));
    };
    assert_eq!(contents.root.as_str(), "disk0/project");
    let names: Vec<_> = contents
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("disk0/project/nested/empty", EntryKind::Directory),
            ("disk0/project/nested/private.local", EntryKind::File),
            ("disk0/project/nested/report.txt", EntryKind::File),
        ]
    );
    let WorkspacePage::Bundle(empty) = workspace.browse(&bundle.join("nested/empty"))? else {
        return Err(invalid("expected contents of the empty bundle directory"));
    };
    assert!(empty.entries.is_empty());

    let report = bundle.join("nested/report.txt");
    workspace.db.save(&fixture_track(
        "disk0/project/nested/report.txt",
        Snapshot::File(stamp(&report)?),
    )?)?;
    assert!(
        workspace
            .status()?
            .bundle_warnings
            .contains(&LocalPath::parse("disk0/project")?)
    );
    assert!(matches!(
        workspace.browse(&bundle)?,
        WorkspacePage::Changes(_)
    ));
    Ok(())
}

#[test]
fn nested_manifests_keep_the_outer_bundle_as_the_submission_root() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let project = root.join("disk0/project");
    let nested = project.join("nested");
    fs::create_dir_all(&nested)?;
    fs::write(project.join(".zxsignore"), b"")?;
    fs::write(nested.join(".zxsignore"), b"")?;
    fs::write(nested.join("report.txt"), b"report")?;

    let workspace = Workspace::open(&root)?;
    let status = workspace.status()?;
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].path.as_str(), "disk0/project");
    let WorkspacePage::Bundle(page) = workspace.browse(&nested)? else {
        return Err(invalid("expected contents of the outer bundle"));
    };
    assert_eq!(page.root.as_str(), "disk0/project");
    assert_eq!(page.entries.len(), 2);
    Ok(())
}

#[test]
fn interrupted_bundle_checkout_can_list_partial_local_contents() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let target = root.join("disk0/recover");
    fs::create_dir_all(&target)?;
    fs::write(target.join("partial.txt"), b"published before interruption")?;

    let workspace = Workspace::open(&root)?;
    let path = LocalPath::parse("disk0/recover")?;
    workspace.db.start_checkout(&InterruptedCheckout {
        path: path.clone(),
        kind: EntryKind::Directory,
        container_id: ContainerId::parse("c".repeat(32))?,
        version: VersionNum::new(2)?,
        endpoint: "http://127.0.0.1".into(),
        payload_hash: None,
        restored_against: None,
        backup: None,
    })?;

    let statuses = change_rows(&workspace, "disk0")?;
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].status.bundle);
    assert!(matches!(statuses[0].status.state, FileState::Interrupted));
    let WorkspacePage::Bundle(page) = workspace.browse(&target)? else {
        return Err(invalid(
            "expected locally available contents of the interrupted checkout",
        ));
    };
    assert_eq!(page.root, path);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].path.as_str(), "disk0/recover/partial.txt");
    Ok(())
}

#[cfg(unix)]
#[test]
fn absolute_workspace_aliases_keep_child_symlink_checks() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let alias = temporary.child("alias");
    fs::create_dir_all(root.join("disk0"))?;
    symlink(&root, &alias)?;
    symlink(&root, root.join("loop"))?;

    let workspace = Workspace::open(&root)?;
    let relative = Path::new("disk0/new.txt");
    assert_eq!(
        workspace.local_path(&alias.join(relative))?,
        workspace.local_path(relative)?
    );
    assert!(matches!(
        workspace.local_path(&alias.join("loop/disk0/new.txt")),
        Err(super::Error::SymbolicLink(_))
    ));
    assert!(
        workspace
            .local_path(&alias.join("disk0/../new.txt"))
            .is_err()
    );
    Ok(())
}

fn change_rows(workspace: &Workspace, folder: &str) -> Result<Vec<WorkspaceEntry>> {
    let WorkspacePage::Changes(changes) = workspace.browse(Path::new(folder))? else {
        return Err(invalid(format!("expected workspace changes for {folder}")));
    };
    Ok(changes.entries)
}

#[test]
fn recorded_snapshot_keeps_new_local_edits_dirty() -> Result<()> {
    for edit_after_send in [false, true] {
        let temporary = TempDir::create(&std::env::temp_dir())?;
        let root = temporary.child("workspace");
        fs::create_dir(&root)?;
        let workspace = Workspace::open(&root)?;
        let path = root.join("draft.txt");
        fs::write(&path, b"first")?;
        let tracked = fixture_track("draft.txt", Snapshot::File(stamp(&path)?))?;
        if edit_after_send {
            fs::write(&path, b"new changes after request")?;
        }
        workspace.db.save(&tracked)?;
        let statuses = workspace.status()?.entries;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].version, Some(VersionNum::new(1)?));
        assert_eq!(
            matches!(statuses[0].state, FileState::Modified),
            edit_after_send
        );
    }
    Ok(())
}

#[test]
fn tracked_paths_detect_case_insensitive_overlap() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let folder = root.join("disk1/Docs");
    fs::create_dir_all(&folder)?;
    let file = folder.join("tracked.txt");
    fs::write(&file, b"first")?;
    let workspace = Workspace::open(&root)?;
    let track = fixture_track("disk1/Docs/tracked.txt", Snapshot::File(stamp(&file)?))?;
    workspace.db.save(&track)?;
    assert!(
        workspace
            .unoccupied(&LocalPath::parse("disk1/docs")?)
            .is_err()
    );
    assert!(
        workspace
            .unoccupied(&LocalPath::parse("disk1/Docs-neighbor")?)
            .is_ok()
    );
    assert!(workspace.unoccupied(&track.path).is_ok());

    Ok(())
}

#[test]
fn ignored_bundle_files_do_not_prevent_moving_the_directory() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    let bundle = root.join("disk1/project");
    fs::create_dir_all(&bundle)?;
    fs::write(bundle.join(".zxsignore"), b"*.local\n")?;
    let workspace = Workspace::open(&root)?;
    let mut track = fixture_track(
        "disk1/project",
        Snapshot::Bundle(crate::zip_bundle::collect(&bundle)?),
    )?;
    track.hashes = ContentHashes::Bundle(MemberHashes::from([(
        LocalPath::parse(".zxsignore")?,
        crate::store::digest::file(&bundle.join(".zxsignore"))?,
    )]));
    workspace.db.save(&track)?;
    fs::write(bundle.join("draft.local"), b"local data")?;
    let target = LocalPath::parse("disk1/renamed")?;
    workspace.relocate(&mut track, &target)?;
    assert_eq!(
        fs::read(root.join("disk1/renamed/draft.local"))?,
        b"local data"
    );
    Ok(())
}

#[test]
fn interrupted_case_rename_restores_the_original_path() -> Result<()> {
    let temporary = TempDir::create(&std::env::temp_dir())?;
    let root = temporary.child("workspace");
    fs::create_dir_all(root.join("disk1"))?;
    let workspace = Workspace::open(&root)?;
    let stage = root.join(".zxs/case-move");
    fs::create_dir(&stage)?;
    fs::write(
        stage.join("source.json"),
        br#"{"source":"disk1/Original.txt"}"#,
    )?;
    fs::write(stage.join("payload"), b"local data")?;
    drop(workspace);
    let _workspace = Workspace::open(&root)?;
    assert_eq!(fs::read(root.join("disk1/Original.txt"))?, b"local data");
    assert!(!stage.exists());
    Ok(())
}

fn fixture_track(path: &str, snapshot: Snapshot) -> Result<Track> {
    let hashes = if matches!(snapshot, Snapshot::Bundle(_)) {
        ContentHashes::Bundle(MemberHashes::new())
    } else {
        ContentHashes::File(Blake3Hash::parse(
            blake3::hash(b"first").to_hex().to_string(),
        )?)
    };
    Ok(Track {
        path: LocalPath::parse(path)?,
        container_id: ContainerId::parse("b".repeat(32))?,
        version: VersionNum::new(1)?,
        endpoint: "http://127.0.0.1".into(),
        snapshot,
        hashes,
        mirrored: true,
        remote_missing: false,
        remote_blake3: None,
        restored_from: None,
    })
}
