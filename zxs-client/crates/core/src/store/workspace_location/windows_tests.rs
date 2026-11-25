use std::{fs, io::ErrorKind, thread, time::Duration};

use super::{
    FileName, Result, TempDir, configured_workspace_in, move_workspace_in, rename_directory,
};
use crate::{Workspace, domain::invalid};

#[test]
fn rename_retries_while_a_windows_file_handle_is_released() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let original = temp.child("source");
    let target = temp.child("destination");
    fs::create_dir(&original)?;
    let draft = original.join("draft.txt");
    fs::write(&draft, b"pending changes")?;
    let held = fs::File::open(&draft)?;
    let blocked = fs::rename(&original, &target)
        .err()
        .map(|error| error.kind());
    assert_eq!(blocked, Some(ErrorKind::PermissionDenied));

    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(125));
        drop(held);
    });
    let result = rename_directory(&original, &target);
    release
        .join()
        .map_err(|_| invalid("file handle release failed"))?;
    result?;
    assert!(!original.exists());
    assert_eq!(fs::read(target.join("draft.txt"))?, b"pending changes");
    Ok(())
}

#[test]
fn a_persistently_held_file_leaves_the_original_workspace_ready_to_retry() -> Result<()> {
    let temp = TempDir::create(&std::env::temp_dir())?;
    let settings = temp.child("settings");
    let original = temp.child("source");
    let target = temp.child("destination");
    let parent = super::parent(&original)?;
    fs::create_dir(&original)?;
    let old = move_workspace_in(&settings, &original, parent, &FileName::parse("source")?)?;
    let draft = original.join("draft.txt");
    fs::write(&draft, b"pending changes")?;
    let held = fs::File::open(&draft)?;
    let name = FileName::parse("destination")?;
    let Err(error) = move_workspace_in(&settings, &original, parent, &name) else {
        return Err(invalid(
            "could move the directory while a child file was open",
        ));
    };
    assert!(
        error
            .to_string()
            .contains("请关闭正在使用该目录或其中文件的程序")
    );
    assert_eq!(configured_workspace_in(&settings)?, Some(old.path));
    assert!(!target.exists());
    Workspace::open(&original)?;

    drop(held);
    let moved = move_workspace_in(&settings, &original, parent, &name)?;
    assert_eq!(configured_workspace_in(&settings)?, Some(moved.path));
    assert_eq!(fs::read(target.join("draft.txt"))?, b"pending changes");
    assert!(!original.exists());
    Ok(())
}
