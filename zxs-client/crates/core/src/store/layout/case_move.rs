use std::{
    fs,
    io::{self, ErrorKind, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::domain::{EntryKind, Error, LocalPath, Result, invalid};

use super::{Layout, TempDir, kind, publication::rename_new, sync_parent};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    source: LocalPath,
}

impl Layout {
    pub fn rename_case(&self, source: &Path, target: &Path) -> Result<()> {
        let directory = self.system.join("case-move");
        fs::create_dir(&directory)?;
        let mut stage = TempDir {
            path: directory,
            cleanup: true,
        };
        let note_path = stage.child("source.json");
        let mut note = fs::File::create_new(&note_path)?;
        note.write_all(&serde_json::to_vec(&Move {
            source: self.local(source)?,
        })?)?;
        note.sync_all()?;
        drop(note);
        sync_parent(&note_path)?;
        sync_parent(&stage.path)?;

        let payload = stage.child("payload");
        fs::rename(source, &payload)?;
        stage.preserve();
        sync_parent(&payload)?;
        sync_parent(source)?;
        let result = rename_new(&payload, target);
        if result.is_err() {
            put_back(&payload, source)?;
            sync_parent(source)?;
        } else {
            sync_parent(target)?;
        }
        stage.cleanup = true;
        result.map_err(|error| move_error(error, target, source))
    }

    pub(super) fn recover_case_move(&self) -> Result<()> {
        let directory = self.system.join("case-move");
        match kind(&directory)? {
            None => return Ok(()),
            Some(EntryKind::File) => return Err(invalid("invalid workspace move stage")),
            Some(EntryKind::Directory) => {}
        }
        let payload = directory.join("payload");
        if kind(&payload)?.is_some() {
            let bytes = super::read_small(&directory.join("source.json"))?;
            let planned: Move = serde_json::from_slice(&bytes)?;
            let original = self.path(&planned.source)?;
            put_back(&payload, &original)?;
            sync_parent(&original)?;
        }
        fs::remove_dir_all(&directory)?;
        sync_parent(&directory)
    }
}

fn put_back(payload: &Path, destination: &Path) -> Result<()> {
    rename_new(payload, destination).map_err(|error| move_error(error, destination, payload))
}

fn move_error(error: io::Error, destination: &Path, preserved: &Path) -> Error {
    if error.kind() == ErrorKind::AlreadyExists {
        return Error::LocalConflict(format!(
            "本地目标 {} 已存在，移动的内容保留于 {}",
            destination.display(),
            preserved.display()
        ));
    }
    error.into()
}
