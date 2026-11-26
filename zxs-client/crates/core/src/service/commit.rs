use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{Blake3Hash, ContentHashes, MemberHashes, Revision, Stamp, Status, revision_conflict},
    store::layout::{TempDir, check_unique_name, stamp},
    transport::UploadFile,
};

use super::{
    Catalog, Commit, Container, EntryKind, Error, FileName, FolderPath, LocalPath, NewContainer,
    Remote, Result, Snapshot, Track, Workspace, invalid, kind, zip_bundle,
};

mod conflict;
mod directory;

pub use conflict::{UploadConflict, UploadLocation};

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

enum Target<'a> {
    Existing {
        container: &'a Container,
        baseline: Revision,
    },
    New(NewContainer),
}

struct Contents {
    path: PathBuf,
    snapshot: Snapshot,
    members: MemberHashes,
    _stage: Option<TempDir>,
}

impl Contents {
    fn upload(&self, checked: Option<(Stamp, Blake3Hash)>) -> Result<UploadFile> {
        let mut file = UploadFile::open(&self.path)?;
        if let (Some((verified, hash)), Snapshot::File(snapshot)) = (checked, &self.snapshot)
            && verified == *snapshot
        {
            file.hash = Some(hash);
        }
        Ok(file)
    }
}

struct CommitJob<'a> {
    remote: &'a Remote,
    contents: &'a Contents,
    local: &'a LocalPath,
    file: UploadFile,
}

impl CommitJob<'_> {
    fn check_source(&self) -> Result<()> {
        if let Snapshot::File(previous) = &self.contents.snapshot
            && (self.file.size != previous.size || stamp(&self.contents.path)? != *previous)
        {
            return Err(Error::LocalConflict(
                "local file changed while preparing upload".into(),
            ));
        }
        Ok(())
    }
}

impl Workspace {
    /// # Errors
    /// Returns an error if preparation, upload, tracking, or the selected conflict resolution fails.
    pub fn commit(&self, remote: &Remote, request: &CommitRequest) -> Result<CommitOutcome> {
        self.commit_entry(remote, request, None)
    }

    fn commit_entry(
        &self,
        remote: &Remote,
        request: &CommitRequest,
        scanned: Option<&Status>,
    ) -> Result<CommitOutcome> {
        let local = scanned.map_or_else(
            || self.layout.local(&request.path),
            |entry| Ok(entry.path.clone()),
        )?;
        let path = self.layout.path(&local)?;
        if scanned.is_none() {
            check_unique_name(&path)?;
        }
        let tracked = self.commit_track(remote, &local)?;
        let save_as = matches!(request.on_conflict, ConflictChoice::New);
        if save_as && request.name.is_none() && request.folder.is_none() {
            return Err(invalid("saving as new requires --name or --folder"));
        }
        let conflict = if save_as {
            None
        } else {
            self.moved_commit_conflict(tracked.as_ref())?
        };
        if matches!(request.on_conflict, ConflictChoice::Stop)
            && let Some(error) = conflict
        {
            return Err(error);
        }
        let contents = self.prepare(&path, &local, remote, tracked.is_some())?;
        let mut job = CommitJob {
            remote,
            contents: &contents,
            local: &local,
            file: contents.upload(scanned.and_then(|entry| entry.file_digest.clone()))?,
        };
        if let Some((track, _)) = &tracked
            && matches!(track.snapshot, Snapshot::Bundle(_))
            && matches!(contents.snapshot, Snapshot::File(_))
        {
            return Err(Error::LocalConflict(
                "本地路径原为目录包，请检出后再提交目录".into(),
            ));
        }
        let target = match &tracked {
            Some((track, container)) if !save_as => Target::Existing {
                container,
                baseline: track.baseline(),
            },
            _ => Target::New(new_container(
                request,
                &local,
                &contents.snapshot,
                tracked.as_ref().map(|(_, container)| container),
            )?),
        };
        if let Some(error) = conflict {
            return self.handle_conflict(&mut job, request, &target, error);
        }
        match self.send(&mut job, &target) {
            Ok(commit) => Ok(CommitOutcome::Committed(Box::new(commit))),
            Err(Error::Remote { status: 404, .. }) if matches!(target, Target::Existing { .. }) => {
                Err(self.mark_missing(&local)?)
            }
            Err(error @ Error::Remote { status: 409, .. }) => {
                self.handle_conflict(&mut job, request, &target, error)
            }
            Err(error) => Err(error),
        }
    }

    fn commit_track(
        &self,
        remote: &Remote,
        local: &LocalPath,
    ) -> Result<Option<(Track, Container)>> {
        let track = self.db.track(local)?;
        if self.db.all_checkouts()?.iter().any(|entry| {
            *local == entry.path
                || local.below(&entry.path)
                || entry.path.below(local)
                || track
                    .as_ref()
                    .is_some_and(|track| track.container_id == entry.container_id)
        }) {
            return Err(Error::CheckoutInterrupted);
        }
        let Some(track) = track else { return Ok(None) };
        match self.tracked_container(remote, &track) {
            Err(Error::Remote { status: 404, .. }) => Err(self.mark_missing(local)?),
            result => Ok(Some((track, result?))),
        }
    }

    fn prepare(
        &self,
        path: &Path,
        local: &LocalPath,
        remote: &Remote,
        tracked: bool,
    ) -> Result<Contents> {
        let entry = kind(path)?;
        if entry == Some(EntryKind::Directory) {
            if !tracked {
                super::bundle_workspace::check_bundle_location(
                    local,
                    &Catalog::open(remote, None)?,
                )?;
            }
            self.check_bundle_directory(local, remote)?;
        } else {
            self.unoccupied(local)?;
        }
        match entry {
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

    fn send(&self, job: &mut CommitJob<'_>, target: &Target<'_>) -> Result<Commit> {
        if let Target::Existing {
            container,
            baseline,
        } = target
        {
            if baseline.version != container.latest_version {
                return Err(Error::Remote {
                    status: 409,
                    message: format!(
                        "expected version {}, latest is {}",
                        baseline.version, container.latest_version
                    ),
                });
            }
            if revision_conflict(baseline.version, baseline.blake3.as_ref(), container) {
                return Err(Error::Remote {
                    status: 409,
                    message: "远端同号版本内容已变更".into(),
                });
            }
            if matches!(job.contents.snapshot, Snapshot::Bundle(_))
                && zip_bundle::zip_stem(container.name.as_str()).is_none()
            {
                return Err(invalid("目录包名称须以 .zip 结尾"));
            }
        }
        if matches!(job.contents.snapshot, Snapshot::File(_)) {
            job.file.ensure_digest()?;
            job.check_source()?;
        }
        let commit = match target {
            Target::Existing {
                container,
                baseline,
            } => job
                .remote
                .append(&container.container_id, baseline.version, &mut job.file),
            Target::New(new) => job.remote.create(new, &mut job.file),
        }?;
        self.record_commit(job, &commit)?;
        Ok(commit)
    }

    fn record_commit(&self, job: &CommitJob<'_>, commit: &Commit) -> Result<()> {
        let bundle = matches!(job.contents.snapshot, Snapshot::Bundle(_));
        let mirror = self.destination(&commit.container)?;
        let mirrored = mirror == *job.local
            || (bundle && zip_bundle::matches_bundle_stem(job.local, &mirror))
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
            remote_blake3: bundle.then(|| commit.version.blake3.clone()),
            restored_from: None,
            snapshot: job.contents.snapshot.clone(),
            hashes: if bundle {
                ContentHashes::Bundle(job.contents.members.clone())
            } else {
                ContentHashes::File(commit.version.blake3.clone())
            },
        })
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
    let name = if matches!(snapshot, Snapshot::Bundle(_)) {
        zip_bundle::bundle_name(&name)?
    } else {
        name
    };
    let tags = if request.tags.is_empty() {
        original.map_or_else(Vec::new, |c| c.tags.clone())
    } else {
        request.tags.clone()
    };
    Ok(NewContainer { folder, name, tags })
}
