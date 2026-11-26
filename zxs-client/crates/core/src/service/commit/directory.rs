use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use serde::Serialize;

use super::{
    Catalog, CommitRequest, ConflictChoice, EntryKind, Error, FolderPath, LocalPath, Remote,
    Result, Workspace, invalid, kind,
};
use crate::domain::{Folder, Status, file_name_key};
use crate::service::browse::{DirectoryScan, ScanPurpose, pending_commit};

#[derive(Default, Serialize)]
pub struct DirectoryCommitReport {
    committed: usize,
    created_folders: usize,
    remaining: usize,
    issues: Vec<CommitIssue>,
    bundle_warnings: Vec<LocalPath>,
    symlink_warnings: Vec<String>,
}

#[derive(Serialize)]
struct CommitIssue {
    path: LocalPath,
    reason: String,
    conflict: bool,
}

impl Workspace {
    /// Submits changes within an ordinary directory using the existing per-file commit rules.
    ///
    /// # Errors
    /// Returns an error if the scope is invalid or cannot be scanned. Individual submission errors
    /// are reported with their paths so previously completed items can be safely retried.
    pub fn commit_directory(
        &self,
        remote: &Remote,
        directory: &Path,
    ) -> Result<DirectoryCommitReport> {
        let directory = self.layout.local(directory)?;
        if directory.is_root() {
            return Err(invalid("请进入数据盘或子目录再提交"));
        }
        let folder = remote_folder(&directory)?;
        Catalog::open(remote, None)?.check_disk(&folder)?;
        match kind(&self.layout.path(&directory)?)? {
            None => return Ok(DirectoryCommitReport::default()),
            Some(EntryKind::File) => return Err(invalid("请选择目录提交")),
            Some(EntryKind::Directory) => {}
        }
        if self.bundle_root(&directory)?.is_some() {
            return Err(invalid("请在工作区打开该目录包并提交"));
        }

        let DirectoryScan {
            mut status,
            empty_directories,
            name_conflicts,
        } = self.scan_directory(&directory, ScanPurpose::Commit)?;
        let empty_directories = missing_folders(remote, &folder, empty_directories)?;
        status.entries.retain(pending_commit);
        let count = status.entries.len() + empty_directories.len();
        let uploads = status
            .entries
            .iter()
            .map(|entry| (&entry.path, Some(entry)));
        let folders = empty_directories.iter().map(|path| (path, None));
        let mut result = DirectoryCommitReport {
            bundle_warnings: status.bundle_warnings,
            symlink_warnings: status.symlink_warnings,
            ..DirectoryCommitReport::default()
        };
        for (index, (path, entry)) in uploads.chain(folders).enumerate() {
            let outcome = match entry {
                Some(entry) => self
                    .submit_scanned_file(remote, entry, &name_conflicts)
                    .map(|()| result.committed += 1),
                None => self
                    .submit_empty_folder(remote, path)
                    .map(|created| result.created_folders += usize::from(created)),
            };
            if let Err(error) = outcome {
                result.issues.push(CommitIssue {
                    path: path.clone(),
                    reason: error.to_string(),
                    conflict: matches!(error, Error::CommitConflict(_)),
                });
                if matches!(
                    error,
                    Error::Http(_)
                        | Error::PasswordRejected(_)
                        | Error::Remote {
                            status: 401 | 403,
                            ..
                        }
                ) {
                    result.remaining = count - index - 1;
                    break;
                }
            }
        }
        Ok(result)
    }

    fn submit_scanned_file(
        &self,
        remote: &Remote,
        entry: &Status,
        name_conflicts: &HashSet<(LocalPath, String)>,
    ) -> Result<()> {
        let path = &entry.path;
        if let Some(parent) = path.parent() {
            let name = path.as_str().rsplit('/').next().unwrap_or_default();
            if name_conflicts.contains(&(parent, file_name_key(name))) {
                return Err(invalid(format!("multiple canonical spellings of {name}")));
            }
        }
        self.commit_entry(
            remote,
            &CommitRequest {
                path: Path::new(path.as_str()).into(),
                folder: None,
                name: None,
                tags: Vec::new(),
                on_conflict: ConflictChoice::Stop,
            },
            Some(entry),
        )?;
        Ok(())
    }

    fn submit_empty_folder(&self, remote: &Remote, path: &LocalPath) -> Result<bool> {
        if kind(&self.layout.path(path)?)? != Some(EntryKind::Directory) {
            return Ok(false);
        }
        let (_, created) = remote.create_folder(&Folder {
            folder: remote_folder(path)?,
        })?;
        Ok(created)
    }
}

fn remote_folder(path: &LocalPath) -> Result<FolderPath> {
    FolderPath::parse(format!("/{}", path.as_str()))
}

fn missing_folders(
    remote: &Remote,
    scope: &FolderPath,
    mut paths: Vec<LocalPath>,
) -> Result<Vec<LocalPath>> {
    if !paths.is_empty() {
        match remote.descendant_folders(scope) {
            Ok(page) => {
                let mut existing = BTreeSet::from([scope.as_str().trim_start_matches('/')]);
                existing.extend(
                    page.folders
                        .iter()
                        .map(|entry| entry.folder.as_str().trim_start_matches('/')),
                );
                paths.retain(|path| !existing.contains(path.as_str()));
            }
            Err(Error::Remote { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(paths)
}
