use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    Commit, Container, ContainerId, EntryKind, Error, FileName, FolderPath, LocalPath,
    NewContainer, PendingCommit, Remote, Result, Snapshot, Track, VersionNum, Workspace,
    check_endpoint, invalid, kind, new_op, normalize_tags, zip_bundle,
};
use crate::{store::layout::TempDir, transport::HashedFile};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictChoice {
    #[default]
    Stop,
    Append,
    New,
    Pull,
}

pub struct CommitRequest {
    pub path: PathBuf,
    pub folder: Option<FolderPath>,
    pub name: Option<FileName>,
    pub tags: Vec<String>,
    pub on_conflict: ConflictChoice,
}

#[derive(Serialize)]
#[serde(tag = "outcome", content = "data", rename_all = "snake_case")]
pub enum CommitOutcome {
    Committed(Box<Commit>),
    Pulled(super::CheckoutResult),
}

enum Target {
    Existing(ContainerId, VersionNum),
    New(NewContainer),
}

struct Contents {
    path: PathBuf,
    snapshot: Snapshot,
    _stage: Option<TempDir>,
}

struct CommitJob<'a> {
    remote: &'a Remote,
    contents: &'a Contents,
    local: &'a LocalPath,
}

impl Workspace {
    /// # Errors
    ///
    /// Returns an error if preparation, upload, tracking, or the selected conflict resolution fails.
    pub fn commit(&self, remote: &Remote, request: &CommitRequest) -> Result<CommitOutcome> {
        let local = self.layout.local(&request.path)?;
        self.unoccupied(&local)?;
        if let Some(pending) = self.db.pending(&local)?
            && let Some(commit) = self.reconcile(remote, pending)?
        {
            return Ok(CommitOutcome::Committed(Box::new(commit)));
        }
        let tracked = self.db.track(&local)?;
        let path = self.layout.path(&local)?;
        let contents = self.prepare(&path)?;
        let job = CommitJob {
            remote,
            contents: &contents,
            local: &local,
        };
        let target = match &tracked {
            Some(track) => {
                check_endpoint(remote, &track.endpoint)?;
                Target::Existing(track.container_id.clone(), track.version)
            }
            None => Target::New(new_container(request, &path, &contents.snapshot, None)?),
        };
        match self.send(&job, target) {
            Err(Error::Remote {
                status: 409,
                message,
            }) => match tracked {
                Some(track) if !matches!(request.on_conflict, ConflictChoice::Stop) => {
                    self.resolve(&job, request, track)
                }
                _ => Err(Error::Remote {
                    status: 409,
                    message,
                }),
            },
            Err(error) => Err(error),
            Ok(commit) => Ok(CommitOutcome::Committed(Box::new(commit))),
        }
    }

    fn prepare(&self, path: &Path) -> Result<Contents> {
        match kind(path)? {
            Some(EntryKind::File) => Ok(Contents {
                path: path.into(),
                snapshot: Snapshot::File(crate::store::layout::stamp(path)?),
                _stage: None,
            }),
            Some(EntryKind::Directory) => {
                let members = zip_bundle::collect(path)?;
                let stage = self.layout.staged()?;
                let archive = stage.child("bundle.zip");
                zip_bundle::pack(path, &members, &archive)?;
                Ok(Contents {
                    path: archive,
                    snapshot: Snapshot::Bundle(members),
                    _stage: Some(stage),
                })
            }
            None => Err(invalid(format!("missing local path {}", path.display()))),
        }
    }

    fn send(&self, job: &CommitJob<'_>, target: Target) -> Result<Commit> {
        let file = HashedFile::open(&job.contents.path)?;
        if let Snapshot::File(stamp) = &job.contents.snapshot
            && (file.size != stamp.size
                || crate::store::layout::stamp(&job.contents.path)? != *stamp)
        {
            return Err(Error::LocalConflict(
                "local file changed while preparing upload".into(),
            ));
        }
        let op_id = new_op()?;
        let pending = PendingCommit {
            op_id: op_id.clone(),
            path: job.local.clone(),
            endpoint: job.remote.endpoint().into(),
            snapshot: job.contents.snapshot.clone(),
        };
        self.db.start(&pending)?;
        let result = match target {
            Target::Existing(id, expected) => job.remote.append(op_id, &id, expected, file),
            Target::New(new) => job.remote.create(op_id, &new, file),
        };
        match result {
            Ok(commit) => {
                self.record_commit(pending, &commit)?;
                Ok(commit)
            }
            Err(error @ Error::Uncertain { .. }) => Err(error),
            Err(error) => {
                self.db.abandon(job.local)?;
                Err(error)
            }
        }
    }

    fn resolve(
        &self,
        job: &CommitJob<'_>,
        request: &CommitRequest,
        track: Track,
    ) -> Result<CommitOutcome> {
        let remote = job.remote;
        let container = remote.detail(&track.container_id)?.container;
        let target = match request.on_conflict {
            ConflictChoice::Append => {
                Target::Existing(track.container_id, container.latest_version)
            }
            ConflictChoice::New => {
                if request.name.is_none() && request.folder.is_none() {
                    return Err(invalid("saving as new requires --name or --folder"));
                }
                let path = self.layout.path(job.local)?;
                Target::New(new_container(
                    request,
                    &path,
                    &job.contents.snapshot,
                    Some(&container),
                )?)
            }
            ConflictChoice::Pull => {
                let checkout = self.checkout(
                    remote,
                    super::CheckoutRequest {
                        id: track.container_id,
                        version: None,
                        destination: Some(request.path.clone()),
                        choice: super::Checkout::Backup,
                    },
                )?;
                return Ok(CommitOutcome::Pulled(checkout));
            }
            ConflictChoice::Stop => return Err(invalid("conflict requires a resolution")),
        };
        self.send(job, target)
            .map(|commit| CommitOutcome::Committed(Box::new(commit)))
    }
}

fn new_container(
    request: &CommitRequest,
    path: &Path,
    snapshot: &Snapshot,
    original: Option<&Container>,
) -> Result<NewContainer> {
    let folder = request
        .folder
        .clone()
        .or_else(|| original.map(|c| c.folder.clone()))
        .unwrap_or_else(FolderPath::root);
    let name = match (&request.name, original) {
        (Some(name), _) => name.clone(),
        (None, Some(container)) => container.name.clone(),
        (None, None) => {
            let name = path
                .file_name()
                .and_then(|f| f.to_str())
                .ok_or_else(|| invalid("provide a remote file name"))?;
            FileName::parse(if matches!(snapshot, Snapshot::Bundle(_)) {
                format!("{name}.zip")
            } else {
                name.into()
            })?
        }
    };
    let tags = if request.tags.is_empty() {
        original.map_or_else(Vec::new, |c| c.tags.clone())
    } else {
        request.tags.clone()
    };
    Ok(NewContainer {
        folder,
        name,
        tags: normalize_tags(tags)?,
    })
}
