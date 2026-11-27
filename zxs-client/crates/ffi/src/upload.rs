use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zxs_core::{
    Commit, Container, ContainerId, Error, NewContainer, Result, TransferObserver, UploadFile,
    UploadLocation, VersionNum,
};

use super::{Connection, backup::Photo};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitFile {
    connection: Connection,
    file: PathBuf,
    target: Target,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    New(NewContainer),
    Version {
        container_id: ContainerId,
        expected_version: VersionNum,
    },
    Photo(Photo),
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum Outcome {
    Committed { commit: Commit },
    AlreadyPresent { current: Container },
    Conflict { current: Container },
}

impl CommitFile {
    pub(super) fn run(self, progress: Option<TransferObserver>) -> Result<Value> {
        let remote = self.connection.open()?.with_progress(progress);
        let mut file = UploadFile::open(&self.file)?;
        let (result, location) = match &self.target {
            Target::New(params) => (
                remote.create(params, &mut file),
                UploadLocation::New(params),
            ),
            Target::Version {
                container_id,
                expected_version,
            } => (
                remote.append(container_id, *expected_version, &mut file),
                UploadLocation::Existing(container_id),
            ),
            Target::Photo(photo) => {
                return Ok(serde_json::to_value(photo.upload(
                    &self.connection,
                    &remote,
                    &mut file,
                )?)?);
            }
        };
        let outcome = match result {
            Ok(commit) => Outcome::Committed { commit },
            Err(error @ Error::Remote { status: 409, .. }) => {
                let Some(conflict) = remote.upload_conflict(location, &mut file)? else {
                    return Err(error);
                };
                if conflict.already_present {
                    Outcome::AlreadyPresent {
                        current: conflict.current,
                    }
                } else {
                    Outcome::Conflict {
                        current: conflict.current,
                    }
                }
            }
            Err(error) => return Err(error),
        };
        Ok(serde_json::to_value(outcome)?)
    }
}
