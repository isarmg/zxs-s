use crate::{
    ContainerId, NewContainer, Remote,
    service::{Checkout, CheckoutRequest},
};

use super::{
    Catalog, Commit, CommitJob, CommitOutcome, CommitRequest, ConflictChoice, Error, Result,
    Revision, Target, Track, UploadFile, Workspace, invalid, new_container,
};

#[derive(Clone, Copy)]
pub enum UploadLocation<'a> {
    New(&'a NewContainer),
    Existing(&'a ContainerId),
}

pub struct UploadConflict {
    pub current: Commit,
    pub already_present: bool,
}

impl Remote {
    /// Looks up the conflicting target and compares its current content with the upload.
    ///
    /// # Errors
    /// Returns an error if the target cannot be read or the upload cannot be hashed.
    pub fn upload_conflict(
        &self,
        location: UploadLocation<'_>,
        file: &mut UploadFile,
    ) -> Result<Option<UploadConflict>> {
        let id = match location {
            UploadLocation::Existing(id) => id.clone(),
            UploadLocation::New(params) => {
                let catalog = Catalog::open(self, None)?;
                let Some(container) = catalog.find_name(&params.folder, &params.name)? else {
                    return Ok(None);
                };
                container.container_id
            }
        };
        let (container, version) = self.version(&id, None)?;
        file.ensure_digest()?;
        Ok(Some(UploadConflict {
            already_present: file.hash.as_ref() == Some(&version.blake3),
            current: Commit { container, version },
        }))
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
    ) -> Result<CommitOutcome> {
        let location = match target {
            Target::Existing { container, .. } => UploadLocation::Existing(&container.container_id),
            Target::New(params) => UploadLocation::New(params),
        };
        let Some(conflict) = job.remote.upload_conflict(location, &mut job.file)? else {
            return Err(error);
        };
        if conflict.already_present && !matches!(request.on_conflict, ConflictChoice::Pull) {
            job.check_source()?;
            self.record_commit(job, &conflict.current)?;
            return Ok(CommitOutcome::Committed(Box::new(conflict.current)));
        }
        let Commit { container, version } = conflict.current;
        let target = match request.on_conflict {
            ConflictChoice::Append => Target::Existing {
                container: &container,
                baseline: Revision::from(&container),
            },
            ConflictChoice::New => {
                if request.name.is_none() && request.folder.is_none() {
                    return Err(invalid("saving as new requires --name or --folder"));
                }
                Target::New(new_container(
                    request,
                    job.local,
                    &job.contents.snapshot,
                    Some(&container),
                )?)
            }
            ConflictChoice::Pull => {
                let id = container.container_id.clone();
                let checkout = self.checkout_version(
                    job.remote,
                    CheckoutRequest {
                        id,
                        version: None,
                        destination: matches!(target, Target::New(_)).then(|| request.path.clone()),
                        choice: Checkout::Backup,
                    },
                    Some((container, version)),
                )?;
                return Ok(CommitOutcome::Pulled(checkout));
            }
            ConflictChoice::Stop => return Err(Error::CommitConflict(error.to_string())),
        };
        let mut file = UploadFile::open(&job.contents.path)?;
        file.hash = job.file.hash.take();
        job.file = file;
        self.send(job, &target)
            .map(|commit| CommitOutcome::Committed(Box::new(commit)))
    }
}
