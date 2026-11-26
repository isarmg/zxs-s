use std::{collections::BTreeSet, path::Path};

use serde::Serialize;

use super::{
    Catalog, CommitRequest, ConflictChoice, EntryKind, Error, FolderPath, LocalPath, Remote,
    Result, Workspace, invalid, kind,
};
use crate::domain::Folder;
use crate::service::browse::{ScanPurpose, pending_commit};

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

        let (mut status, empty_directories) =
            self.scan_directory(&directory, ScanPurpose::Commit)?;
        let empty_directories = missing_folders(remote, &folder, empty_directories)?;
        status.entries.retain(pending_commit);
        let count = status.entries.len() + empty_directories.len();
        let uploads = status
            .entries
            .into_iter()
            .map(|entry| (entry.path, entry.file_digest, false));
        let folders = empty_directories.into_iter().map(|path| (path, None, true));
        let mut result = DirectoryCommitReport {
            bundle_warnings: status.bundle_warnings,
            symlink_warnings: status.symlink_warnings,
            ..DirectoryCommitReport::default()
        };
        for (index, (path, digest, empty_folder)) in uploads.chain(folders).enumerate() {
            let outcome = if empty_folder {
                self.submit_empty_folder(remote, &path)
                    .map(|created| result.created_folders += usize::from(created))
            } else {
                self.commit_with_digest(
                    remote,
                    &CommitRequest {
                        path: Path::new(path.as_str()).into(),
                        folder: None,
                        name: None,
                        tags: Vec::new(),
                        on_conflict: ConflictChoice::Stop,
                    },
                    digest,
                )
                .map(|_| result.committed += 1)
            };
            if let Err(error) = outcome {
                result.issues.push(CommitIssue {
                    path,
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
