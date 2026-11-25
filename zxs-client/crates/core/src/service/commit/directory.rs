use std::path::Path;

use serde::Serialize;

use super::{
    Catalog, CommitRequest, ConflictChoice, EntryKind, Error, FolderPath, LocalPath, Remote,
    Result, Workspace, invalid, kind,
};
use crate::service::browse::pending_commit;

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
        let catalog = Catalog::open(remote, None)?;
        catalog.check_disk(&remote_folder(&directory)?)?;
        match kind(&self.layout.path(&directory)?)? {
            None => return Ok(DirectoryCommitReport::default()),
            Some(EntryKind::File) => return Err(invalid("请选择目录提交")),
            Some(EntryKind::Directory) => {}
        }
        if self.bundle_root(&directory)?.is_some() {
            return Err(invalid("请在工作区打开该目录包并提交"));
        }

        let (mut status, empty_directories) = self.scan_directory(&directory, None)?;
        status.entries.retain(pending_commit);
        let count = status.entries.len() + empty_directories.len();
        let uploads = status.entries.into_iter().map(|entry| (entry.path, false));
        let folders = empty_directories.into_iter().map(|path| (path, true));
        let mut result = DirectoryCommitReport {
            bundle_warnings: status.bundle_warnings,
            symlink_warnings: status.symlink_warnings,
            ..DirectoryCommitReport::default()
        };
        for (index, (path, empty_folder)) in uploads.chain(folders).enumerate() {
            let outcome = if empty_folder {
                self.submit_empty_folder(&catalog, &path)
                    .map(|created| result.created_folders += usize::from(created))
            } else {
                self.commit(
                    remote,
                    &CommitRequest {
                        path: Path::new(path.as_str()).into(),
                        folder: None,
                        name: None,
                        tags: Vec::new(),
                        on_conflict: ConflictChoice::Stop,
                    },
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

    fn submit_empty_folder(&self, catalog: &Catalog<'_>, path: &LocalPath) -> Result<bool> {
        if kind(&self.layout.path(path)?)? != Some(EntryKind::Directory) {
            return Ok(false);
        }
        let folder = remote_folder(path)?;
        let parent = folder
            .parent()
            .ok_or_else(|| invalid("请选择数据盘下的目录"))?;
        let exists = || -> Result<bool> {
            Ok(catalog
                .children(&parent)?
                .iter()
                .any(|entry| entry.folder == folder))
        };
        if exists()? {
            return Ok(false);
        }
        match catalog.create_folder(folder.clone()) {
            Err(Error::Remote { status: 409, .. }) if exists()? => Ok(false),
            outcome => outcome.map(|_| true),
        }
    }
}

fn remote_folder(path: &LocalPath) -> Result<FolderPath> {
    FolderPath::parse(format!("/{}", path.as_str()))
}
