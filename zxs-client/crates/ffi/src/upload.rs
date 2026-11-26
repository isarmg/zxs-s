use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    ContainerId, Error, NewContainer, Result, TransferObserver, UploadFile, UploadLocation,
    VersionNum,
};

use super::Connection;

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
}

impl CommitFile {
    pub(super) fn run(self, progress: Option<TransferObserver>) -> Result<Value> {
        let remote = self.connection.transfer(progress)?;
        let mut file = UploadFile::open(&self.file)?.with_digest()?;
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
        };
        match result {
            Ok(commit) => Ok(json!({"outcome": "committed", "commit": commit})),
            Err(error @ Error::Remote { status: 409, .. }) => {
                let Some(conflict) = remote.upload_conflict(location, &mut file)? else {
                    return Err(error);
                };
                if conflict.already_present {
                    Ok(json!({"outcome": "already_present", "commit": conflict.current}))
                } else {
                    Ok(json!({"outcome": "conflict", "current": conflict.current.container}))
                }
            }
            Err(error) => Err(error),
        }
    }
}
