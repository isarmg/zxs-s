use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ContainerId, ContentHashes, MemberHashes, VersionNum},
    store::{
        digest,
        layout::{TempDir, stamp},
    },
    transport::UploadFile,
};

use super::{
    Catalog, Commit, Container, EntryKind, Error, FileName, FolderPath, LocalPath, NewContainer,
    Remote, Result, Snapshot, Track, Workspace, check_endpoint, invalid, kind, zip_bundle,
};

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
    Existing {
        id: ContainerId,
        version: VersionNum,
    },
    New(NewContainer),
}

struct Contents {
    path: PathBuf,
    snapshot: Snapshot,
    members: MemberHashes,
    _stage: Option<TempDir>,
}

struct CommitJob<'a> {
    remote: &'a Remote,
    contents: &'a Contents,
    local: &'a LocalPath,
}

impl Workspace {
    /// # Errors
    /// Returns an error if preparation, upload, tracking, or the selected conflict resolution fails.
    pub fn commit(&self, remote: &Remote, request: &CommitRequest) -> Result<CommitOutcome> {
        let local = self.layout.local(&request.path)?;
        if self.db.all_checkouts()?.iter().any(|entry| {
            local == entry.path || local.below(&entry.path) || entry.path.below(&local)
        }) {
            return Err(Error::CheckoutInterrupted);
        }
        let path = self.layout.path(&local)?;
        let track = self.db.track(&local)?;
        if kind(&path)? == Some(EntryKind::Directory) {
            if track.is_none() {
                super::bundle_workspace::check_bundle_location(
                    &local,
                    &Catalog::open(remote, None)?,
                )?;
            }
            self.check_bundle_directory(&local, remote)?;
        } else {
            self.unoccupied(&local)?;
        }
        let contents = self.prepare(&path)?;
        let job = CommitJob {
            remote,
            contents: &contents,
            local: &local,
        };
        let target = if let Some(track) = &track {
            check_endpoint(remote, &track.endpoint)?;
            if matches!(track.snapshot, Snapshot::Bundle(_))
                && matches!(contents.snapshot, Snapshot::File(_))
            {
                return Err(Error::LocalConflict(
                    "本地路径原为目录包，请检出后再提交目录".into(),
                ));
            }
            Target::Existing {
                id: track.container_id.clone(),
                version: track.version,
            }
        } else {
            Target::New(new_container(request, &local, &contents.snapshot, None)?)
        };
        match self.send(&job, &target) {
            Ok(commit) => Ok(CommitOutcome::Committed(Box::new(commit))),
            Err(Error::Remote { status: 404, .. }) if track.is_some() => {
                Err(self.mark_missing(&local)?)
            }
            Err(error @ Error::Remote { status: 409, .. }) => {
                self.handle_conflict(&job, request, &target, error)
            }
            Err(error) => Err(error),
        }
    }

    fn prepare(&self, path: &Path) -> Result<Contents> {
        match kind(path)? {
            Some(EntryKind::File) => Ok(Contents {
                path: path.into(),
                snapshot: Snapshot::File(stamp(path)?),
                members: MemberHashes::new(),
                _stage: None,
            }),
            Some(EntryKind::Directory) => {
                let members = zip_bundle::collect(path)?;
                let stage = self.layout.staged()?;
                let archive = stage.child("bundle.zip");
                let hashes = zip_bundle::pack(path, &members, &archive)?;
                Ok(Contents {
                    path: archive,
                    snapshot: Snapshot::Bundle(members),
                    members: hashes,
                    _stage: Some(stage),
                })
            }
            None => Err(invalid(format!("missing local path {}", path.display()))),
        }
    }

    fn send(&self, job: &CommitJob<'_>, target: &Target) -> Result<Commit> {
        let catalog = Catalog::open(job.remote, None)?;
        match target {
            Target::New(new) => {
                catalog.validate_destination(
                    &new.folder,
                    &new.name,
                    job.contents.bundle_for(&new.name)?,
                )?;
            }
            Target::Existing { id, .. } => {
                let container = job.remote.detail(id)?.container;
                catalog.validate_destination(
                    &container.folder,
                    &container.name,
                    job.contents.bundle_for(&container.name)?,
                )?;
            }
        }
        let mut file = UploadFile::open(&job.contents.path)?;
        if let Snapshot::File(previous) = &job.contents.snapshot
            && (file.size != previous.size || stamp(&job.contents.path)? != *previous)
        {
            return Err(Error::LocalConflict(
                "local file changed while preparing upload".into(),
            ));
        }
        let commit = match target {
            Target::Existing { id, version } => job.remote.append(id, *version, &mut file),
            Target::New(new) => job.remote.create(new, &mut file),
        }?;
        self.record_commit(job, &commit)?;
        Ok(commit)
    }

    fn handle_conflict(
        &self,
        job: &CommitJob<'_>,
        request: &CommitRequest,
        target: &Target,
        error: Error,
    ) -> Result<CommitOutcome> {
        let id = match target {
            Target::Existing { id, .. } => id.clone(),
            Target::New(params) => {
                let catalog = Catalog::open(job.remote, None)?;
                let Some(container) = catalog.find_name(&params.folder, &params.name)? else {
                    return Err(error);
                };
                container.container_id
            }
        };
        let detail = job.remote.detail(&id)?;
        let current = digest::file(&job.contents.path)?;
        let container = detail.container;
        if container.latest_blake3.as_ref() == Some(&current)
            && let Some(version) = detail
                .versions
                .into_iter()
                .find(|v| v.version == container.latest_version && v.blake3 == current)
        {
            let commit = Commit { container, version };
            self.record_commit(job, &commit)?;
            return Ok(CommitOutcome::Committed(Box::new(commit)));
        }
        if matches!(request.on_conflict, ConflictChoice::Stop) {
            return Err(Error::CommitConflict(error.to_string()));
        }
        self.resolve(job, request, &container)
    }

    fn record_commit(&self, job: &CommitJob<'_>, commit: &Commit) -> Result<()> {
        let bundle = matches!(job.contents.snapshot, Snapshot::Bundle(_));
        let mirror = self.destination(&commit.container, None, bundle)?;
        let mirrored = mirror == *job.local
            || self.db.track(job.local)?.is_some_and(|track| {
                track.mirrored && track.container_id == commit.container.container_id
            });
        self.db.save(&Track {
            path: job.local.clone(),
            container_id: commit.container.container_id.clone(),
            version: commit.version.version,
            endpoint: job.remote.endpoint().into(),
            remote_missing: false,
            mirrored,
            snapshot: job.contents.snapshot.clone(),
            hashes: if bundle {
                ContentHashes::Bundle(job.contents.members.clone())
            } else {
                ContentHashes::File(commit.version.blake3.clone())
            },
        })
    }

    fn resolve(
        &self,
        job: &CommitJob<'_>,
        request: &CommitRequest,
        container: &Container,
    ) -> Result<CommitOutcome> {
        let target = match request.on_conflict {
            ConflictChoice::Append => Target::Existing {
                id: container.container_id.clone(),
                version: container.latest_version,
            },
            ConflictChoice::New => {
                if request.name.is_none() && request.folder.is_none() {
                    return Err(invalid("saving as new requires --name or --folder"));
                }
                Target::New(new_container(
                    request,
                    job.local,
                    &job.contents.snapshot,
                    Some(container),
                )?)
            }
            ConflictChoice::Pull => {
                let checkout = self.checkout(
                    job.remote,
                    super::CheckoutRequest {
                        id: container.container_id.clone(),
                        version: None,
                        destination: Some(request.path.clone()),
                        choice: super::Checkout::Backup,
                    },
                )?;
                return Ok(CommitOutcome::Pulled(checkout));
            }
            ConflictChoice::Stop => return Err(invalid("conflict requires a resolution")),
        };
        self.send(job, &target)
            .map(|commit| CommitOutcome::Committed(Box::new(commit)))
    }
}

impl Contents {
    fn bundle_for(&self, name: &FileName) -> Result<bool> {
        if matches!(self.snapshot, Snapshot::Bundle(_)) {
            return Ok(true);
        }
        Ok(zip_bundle::zip_stem(name.as_str()).is_some() && zip_bundle::header::local(&self.path)?)
    }
}

fn new_container(
    request: &CommitRequest,
    local: &LocalPath,
    snapshot: &Snapshot,
    original: Option<&Container>,
) -> Result<NewContainer> {
    let folder = match request
        .folder
        .as_ref()
        .or_else(|| original.map(|container| &container.folder))
    {
        Some(folder) => folder.clone(),
        None => match local.parent() {
            Some(parent) if !parent.is_root() => {
                FolderPath::parse(format!("/{}", parent.as_str()))?
            }
            _ => FolderPath::root(),
        },
    };
    let name = match (&request.name, original) {
        (Some(name), _) => name.clone(),
        (None, Some(container)) => container.name.clone(),
        (None, None) => {
            let name = local.as_str().rsplit('/').next().unwrap_or_default();
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
    Ok(NewContainer { folder, name, tags })
}
