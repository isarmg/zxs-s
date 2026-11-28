use crate::{
    ContainerId, NewContainer, Remote,
    service::{Checkout, CheckoutRequest},
};

use super::{
    Commit, CommitJob, CommitOutcome, CommitRequest, ConflictChoice, Container, Error, Result,
    Target, Track, Workspace,
};

#[derive(Clone, Copy)]
pub enum UploadLocation<'a> {
    New(&'a NewContainer),
    Existing(&'a ContainerId),
}

impl Remote {
    /// Looks up the current upload target.
    ///
    /// # Errors
    /// Returns an error if the target cannot be read.
    pub fn upload_target(&self, location: UploadLocation<'_>) -> Result<Option<Container>> {
        match location {
            UploadLocation::Existing(id) => self.summary(id).map(Some),
            UploadLocation::New(params) => self.find_name(&params.folder, &params.name),
        }
    }
}

impl Workspace {
    pub(super) fn moved_commit_conflict(
        &self,
        tracked: Option<&(Track, crate::Container)>,
    ) -> Result<Option<Error>> {
        let Some((track, container)) = tracked else {
            return Ok(None);
        };
        let moved = self.moved_mirror(track, container)?;
        Ok(moved
            .filter(|target| target.key() != track.path.key())
            .map(|target| {
                Error::CommitConflict(format!(
                    "本地文件仍在「{}」，服务器上的文件已移至「/{}」",
                    track.path.as_str(),
                    target.as_str()
                ))
            }))
    }

    pub(super) fn handle_conflict(
        &self,
        job: &mut CommitJob<'_>,
        request: &CommitRequest,
        target: &Target<'_>,
        error: Error,
        known: Option<&Container>,
    ) -> Result<CommitOutcome> {
        let location = match target {
            Target::Existing(container) => UploadLocation::Existing(&container.container_id),
            Target::New(params) => UploadLocation::New(params),
        };
        if matches!(request.on_conflict, ConflictChoice::Pull) {
            let id = match location {
                UploadLocation::Existing(id) => id.clone(),
                UploadLocation::New(_) => {
                    let Some(container) = job.remote.upload_target(location)? else {
                        return Err(error);
                    };
                    container.container_id
                }
            };
            let checkout = self.checkout(
                job.remote,
                CheckoutRequest {
                    id,
                    version: None,
                    destination: matches!(target, Target::New(_)).then(|| request.path.clone()),
                    choice: Checkout::Backup,
                },
            )?;
            return Ok(CommitOutcome::Pulled(checkout));
        }
        let current = match known {
            Some(container) => Some(container.clone()),
            None => job.remote.upload_target(location)?,
        };
        let Some(current) = current else {
            return Err(error);
        };
        let already_present = job.file.matches_digest(&current.latest_blake3)?;
        if matches!(target, Target::New(_)) {
            if let Some(track) = self.db.track_for_container(&current.container_id)?
                && track.path != *job.local
            {
                return Err(Error::CommitConflict(format!(
                    "该文件已在工作区追踪为「{}」，请在那里提交",
                    track.path.as_str()
                )));
            }
            if self
                .db
                .interrupted_checkout(&current.container_id)?
                .is_some()
            {
                return Err(Error::CheckoutInterrupted);
            }
        }
        if already_present {
            if matches!(job.contents.snapshot, crate::domain::Snapshot::Bundle(_)) {
                crate::zip_bundle::require_bundle_suffix(current.name.as_str())?;
            }
            let (container, version) = job.remote.version(&current.container_id, None)?;
            if current.folder != container.folder
                || current.name.key() != container.name.key()
                || current.latest_blake3 != version.blake3
            {
                return Err(Error::CommitConflict(
                    "远端文件已更新或移动，请重新提交".into(),
                ));
            }
            let commit = Commit { container, version };
            job.check_source()?;
            self.record_commit(job, &commit)?;
            return Ok(CommitOutcome::Committed(Box::new(commit)));
        }
        if matches!(request.on_conflict, ConflictChoice::Append) {
            return self.send(job, &Target::Existing(&current));
        }
        Err(Error::CommitConflict(error.to_string()))
    }
}
