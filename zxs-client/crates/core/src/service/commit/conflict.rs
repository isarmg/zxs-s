use crate::{
    CommitTarget, invalid,
    service::{Checkout, CheckoutRequest},
};

use super::{
    Commit, CommitJob, CommitOutcome, CommitRequest, ConflictChoice, Container, Error, Result,
    Target, Workspace,
};

fn changed_same_name() -> Error {
    invalid("确认的同名文件已变化，请重新提交")
}

impl Workspace {
    pub(super) fn handle_conflict(
        &self,
        job: &mut CommitJob<'_>,
        request: &CommitRequest,
        target: &Target<'_>,
        error: Option<Error>,
        known: Option<&Container>,
    ) -> Result<CommitOutcome> {
        let pull = matches!(request.on_conflict, ConflictChoice::Pull);
        let current = match (known, target) {
            (Some(container), _) => Some(container.clone()),
            (_, Target::Existing(container)) if pull => Some((*container).clone()),
            (_, Target::Existing(container)) => Some(job.remote.summary(&container.container_id)?),
            (_, Target::New(params)) => job.remote.find_name(&params.folder, &params.name)?,
        };
        let Some(current) = current else {
            return Err(error.unwrap_or_else(changed_same_name));
        };
        if !matches!(request.on_conflict, ConflictChoice::New)
            && let Some(CommitTarget::SameName { same_name }) = &request.expected_container
            && same_name != &current.container_id
        {
            return Err(changed_same_name());
        }
        if pull {
            let checkout = self.checkout(
                job.remote,
                CheckoutRequest {
                    id: current.container_id,
                    version: None,
                    destination: matches!(target, Target::New(_)).then(|| request.path.clone()),
                    choice: Checkout::Backup,
                },
            )?;
            return Ok(CommitOutcome::Pulled(checkout));
        }
        let conflict_target = match target {
            Target::Existing(_) => CommitTarget::Tracked(current.container_id.clone()),
            Target::New(_) => CommitTarget::SameName {
                same_name: current.container_id.clone(),
            },
        };
        let already_present = job.file.matches_digest(&current.latest_blake3)?;
        if matches!(target, Target::New(_)) {
            if let Some(track) = self.db.track_for_container(&current.container_id)?
                && track.path != *job.local
            {
                let message = format!(
                    "该文件已在工作区追踪为「{}」，请在那里提交",
                    track.path.as_str()
                );
                return Err(Error::CommitConflict(message, conflict_target));
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
                    conflict_target,
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
        let message = error.map_or_else(|| "服务器上已有同名文件".into(), |err| err.to_string());
        Err(Error::CommitConflict(message, conflict_target))
    }
}
