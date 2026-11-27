use std::{collections::BTreeSet, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Result, build, invalid,
    paths::{self, EntryKind},
};

use super::{Disk, DiskKind, Report};

const BACKUP: &str = "originals";

impl Disk {
    fn backup(&self) -> PathBuf {
        self.system.join(BACKUP)
    }

    pub(super) fn commit(&self, pending: Pending) -> Result<Report> {
        paths::sync_dir(&self.root)?;
        paths::sync_dir(&self.system)?;
        let scratch = self.work.join("pending.tmp");
        if paths::exists(&scratch)? {
            fs::remove_file(&scratch)?;
        }
        build::write_json(&scratch, &pending)?;
        paths::relocate(&scratch, &self.pending())?;
        self.finish(pending)
    }

    pub(super) fn finish(&self, pending: Pending) -> Result<Report> {
        pending.validate()?;
        let backup = self.backup();
        if !pending.originals.is_empty() {
            paths::mkdir(&backup)?;
        }
        for name in &pending.originals {
            let saved = backup.join(name);
            if !paths::exists(&saved)? {
                paths::relocate(&self.root.join(name), &saved)?;
            }
        }
        let stage = self.stage();
        for name in &pending.published {
            let source = stage.join(name);
            let target = self.root.join(name);
            if paths::exists(&source)? {
                paths::relocate(&source, &target)?;
            } else if paths::kind(&fs::symlink_metadata(&target)?) != EntryKind::Directory {
                return Err(invalid(format!("目标容器缺失 {}", target.display())));
            }
        }
        let entries = paths::entries(&self.root, true)?;
        let normalized = pending
            .published
            .iter()
            .map(|name| paths::key(name))
            .collect::<BTreeSet<_>>();
        if entries
            .iter()
            .any(|entry| entry.kind != EntryKind::Directory)
            || entries.len() != normalized.len()
            || entries
                .iter()
                .map(|entry| paths::key(&entry.name))
                .collect::<BTreeSet<_>>()
                != normalized
        {
            return Err(invalid("切换时发现磁盘根目录有变动，请保留原件与暂存"));
        }
        self.publish_marker(&pending)?;
        if paths::exists(&stage)? {
            fs::remove_dir(&stage)?;
        }
        fs::remove_file(self.pending())?;
        paths::sync_dir(&self.work)?;
        Ok(Report {
            root: self.root.clone(),
            kind: pending.kind,
            disk_id: pending.disk_id,
            containers: pending.containers,
            originals: (!pending.originals.is_empty()).then_some(backup),
        })
    }

    fn publish_marker(&self, pending: &Pending) -> Result<()> {
        let marker = self.marker();
        let text = format!(
            "kind = {:?}\ndisk_id = {:?}\n",
            pending.kind.label(),
            pending.disk_id
        );
        if paths::exists(&marker)? {
            if fs::read_to_string(&marker)? != text {
                return Err(invalid("盘身份与准备记录不一致"));
            }
            return Ok(());
        }
        let scratch = self.work.join("disk.tmp");
        if paths::exists(&scratch)? {
            fs::remove_file(&scratch)?;
        }
        build::write_bytes(&scratch, text.as_bytes())?;
        paths::relocate(&scratch, &marker)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub(super) kind: DiskKind,
    pub(super) disk_id: String,
    pub(super) originals: Vec<String>,
    pub(super) published: Vec<String>,
    pub(super) containers: usize,
}

impl Pending {
    fn validate(&self) -> Result<()> {
        if self.disk_id.len() != 32
            || !self
                .disk_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("准备记录中的盘身份无效"));
        }
        for name in self.originals.iter().chain(&self.published) {
            paths::name(name)?;
        }
        Ok(())
    }
}
