use std::fs;

use crate::{
    domain::{Blake3Hash, OpId, Version},
    store::layout::{TempDir, stamp},
};

use super::{
    Commit, Container, ContainerId, FileName, FileState, FolderPath, LocalPath, PendingCommit,
    Result, Snapshot, VersionNum, Workspace,
};

#[test]
fn recording_a_receipt_keeps_new_local_edits_dirty() -> Result<()> {
    for edit_after_send in [false, true] {
        let temporary = TempDir::create(&std::env::temp_dir())?;
        let root = temporary.child("workspace");
        fs::create_dir(&root)?;
        let workspace = Workspace::open(&root)?;
        let path = root.join("draft.txt");
        fs::write(&path, b"first")?;
        let pending = PendingCommit {
            op_id: OpId::parse("a".repeat(32))?,
            path: LocalPath::parse("draft.txt")?,
            endpoint: "http://127.0.0.1".into(),
            snapshot: Snapshot::File(stamp(&path)?),
        };
        workspace.db.start(&pending)?;
        if edit_after_send {
            fs::write(&path, b"new changes after request")?;
        }
        let commit = sample_commit(&pending.op_id)?;
        workspace.record_commit(pending, &commit)?;
        let statuses = workspace.status()?;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].version, Some(VersionNum::new(1)?));
        assert_eq!(
            matches!(statuses[0].state, FileState::Modified),
            edit_after_send
        );
    }
    Ok(())
}

fn sample_commit(op: &OpId) -> Result<Commit> {
    let name = FileName::parse("draft.txt")?;
    let number = VersionNum::new(1)?;
    Ok(Commit {
        container: Container {
            container_id: ContainerId::parse("b".repeat(32))?,
            folder: FolderPath::root(),
            name: name.clone(),
            tags: Vec::new(),
            latest_version: number,
            created_at: 1,
            updated_at: 1,
        },
        version: Version {
            version: number,
            op_id: op.clone(),
            created_at: 1,
            name_at_commit: name,
            size: 5,
            blake3: Blake3Hash::parse(blake3::hash(b"first").to_hex().to_string())?,
            payload_file: FileName::parse("payload.txt")?,
        },
    })
}
