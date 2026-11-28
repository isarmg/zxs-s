use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    domain::{Blake3Hash, ContentHashes, MemberHashes, Stamp, Status, revision_conflict},
    store::layout::{TempDir, check_unique_name, stamp},
    transport::UploadFile,
};

use super::{
    Commit, Container, EntryKind, Error, FileName, FolderPath, LocalPath, NewContainer, Remote,
    Result, Snapshot, Track, Workspace, invalid, kind, zip_bundle,
};

mod conflict;
mod directory;

pub use conflict::UploadLocation;

#[derive(Clone, Copy)]
pub enum ConflictChoice {
    Stop,
    Append,
    New,
    Pull,
}

pub struct CommitRequest {
    pub path: PathBuf,
    pub expected_container: Option<crate::ContainerId>,
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
    Existing(&'a Container),
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
    prior: Option<&'a Track>,
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
        let tracked = self.commit_track(remote, &local, request.expected_container.as_ref())?;
        let choice = request.on_conflict;
        let save_as = matches!(choice, ConflictChoice::New);
        if save_as && request.name.is_none() && request.folder.is_none() {
            return Err(invalid("saving as new requires --name or --folder"));
        }
        let conflict = if save_as {
            None
        } else {
            self.moved_commit_conflict(tracked.as_ref())?
        };
        if matches!(choice, ConflictChoice::Stop)
            && let Some(error) = conflict
        {
            return Err(error);
        }
        let prior = tracked.as_ref().map(|(track, _)| track);
        let contents = self.prepare(&path, &local, scanned.is_none().then_some(remote), prior)?;
        let mut job = CommitJob {
            remote,
            contents: &contents,
            local: &local,
            prior,
            file: contents.upload(scanned.and_then(|entry| entry.file_digest.clone()))?,
        };
        if let Some(track) = prior
            && matches!(track.snapshot, Snapshot::Bundle(_))
            && matches!(contents.snapshot, Snapshot::File(_))
        {
            return Err(Error::LocalConflict(
                "本地路径原为目录包，请检出后再提交目录".into(),
            ));
        }
        let target = match &tracked {
            Some((track, container)) if !save_as => {
                let target = Target::Existing(container);
                if let Some(error) = conflict.or_else(|| revision_error(track, container)) {
                    return self.handle_conflict(
                        &mut job,
                        request,
                        &target,
                        error,
                        Some(container),
                    );
                }
                if request.expected_container.is_some() && matches!(choice, ConflictChoice::Pull) {
                    return Err(invalid("此文件已恢复同步，请刷新工作区后重试"));
                }
                target
            }
            _ => Target::New(new_container(
                request,
                &local,
                &contents.snapshot,
                tracked.as_ref().map(|(_, container)| container),
            )?),
        };
        match self.send(&mut job, &target) {
            Err(Error::Remote { status: 404, .. }) if matches!(target, Target::Existing(_)) => {
                Err(self.mark_missing(&local)?)
            }
            Err(error @ Error::Remote { status: 409, .. }) => {
                self.handle_conflict(&mut job, request, &target, error, None)
            }
            result => result,
        }
    }

    fn commit_track(
        &self,
        remote: &Remote,
        local: &LocalPath,
        expected: Option<&crate::ContainerId>,
    ) -> Result<Option<(Track, Container)>> {
        let track = self.track_at(local, expected)?;
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
        disk_check: Option<&Remote>,
        track: Option<&Track>,
    ) -> Result<Contents> {
        match kind(path)? {
            Some(EntryKind::File) => {
                self.check_local_target(local, track.map(|track| &track.container_id))?;
                Ok(Contents {
                    path: path.into(),
                    snapshot: Snapshot::File(stamp(path)?),
                    members: MemberHashes::new(),
                    _stage: None,
                })
            }
            Some(EntryKind::Directory) => {
                zip_bundle::require_bundle_suffix(local.name())?;
                if track.is_none() {
                    super::bundle_workspace::bundle_parent(local, disk_check)?;
                }
                self.check_bundle_directory(local, track)?;
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

    fn send(&self, job: &mut CommitJob<'_>, target: &Target<'_>) -> Result<CommitOutcome> {
        if let Target::Existing(container) = target
            && matches!(job.contents.snapshot, Snapshot::Bundle(_))
            && zip_bundle::zip_stem(container.name.as_str()).is_none()
        {
            return Err(invalid("目录包名称须以 .zip 结尾"));
        }
        if matches!(job.contents.snapshot, Snapshot::File(_)) {
            job.file.ensure_digest()?;
            job.check_source()?;
        }
        let commit = match target {
            Target::Existing(container) => job.remote.append(
                &container.container_id,
                container.latest_version,
                &mut job.file,
            ),
            Target::New(new) => job.remote.create(new, &mut job.file),
        }?;
        self.record_commit(job, &commit)?;
        Ok(CommitOutcome::Committed(Box::new(commit)))
    }

    fn record_commit(&self, job: &CommitJob<'_>, commit: &Commit) -> Result<()> {
        let bundle = matches!(job.contents.snapshot, Snapshot::Bundle(_));
        let mirror = self.destination(&commit.container)?;
        let mirrored = job
            .prior
            .filter(|track| track.container_id == commit.container.container_id)
            .map_or(mirror == *job.local, |track| track.mirrored);
        let track = Track {
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
        };
        self.db.save_track(&track, None)
    }
}

fn revision_error(track: &Track, container: &Container) -> Option<Error> {
    let message = if track.version != container.latest_version {
        format!(
            "expected version {}, latest is {}",
            track.version, container.latest_version
        )
    } else if revision_conflict(track.version, track.baseline_hash(), container) {
        "远端同号版本内容已变更".into()
    } else {
        return None;
    };
    Some(Error::Remote {
        status: 409,
        message,
    })
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
        (None, None) => FileName::parse(local.name())?,
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
