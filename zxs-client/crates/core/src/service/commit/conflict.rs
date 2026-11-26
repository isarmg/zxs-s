use crate::service::{Checkout, CheckoutRequest};

use super::{
    Catalog, Commit, CommitJob, CommitOutcome, CommitRequest, ConflictChoice, Error, Result,
    Revision, Target, UploadFile, Workspace, invalid, new_container,
};

impl Workspace {
    pub(super) fn handle_conflict(
        &self,
        job: &mut CommitJob<'_>,
        request: &CommitRequest,
        target: &Target<'_>,
        error: Error,
    ) -> Result<CommitOutcome> {
        let id = match target {
            Target::Existing { container, .. } => container.container_id.clone(),
            Target::New(params) => {
                let catalog = Catalog::open(job.remote, None)?;
                let Some(container) = catalog.find_name(&params.folder, &params.name)? else {
                    return Err(error);
                };
                container.container_id
            }
        };
        let (container, version) = job.remote.version(&id, None)?;
        job.file.ensure_digest()?;
        if job.file.hash.as_ref() == Some(&version.blake3) {
            job.check_source()?;
            let commit = Commit { container, version };
            self.record_commit(job, &commit)?;
            return Ok(CommitOutcome::Committed(Box::new(commit)));
        }
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
                        destination: Some(request.path.clone()),
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
