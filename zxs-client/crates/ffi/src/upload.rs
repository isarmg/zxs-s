use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zxs_core::{
    Commit, Container, ContainerId, Error, FileName, FolderPath, NewContainer, Remote, Result,
    TransferObserver, UploadFile, UploadLocation, VersionNum,
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
    New {
        folder: FolderPath,
        name: String,
        #[serde(default)]
        tags: Vec<String>,
    },
    Version {
        container_id: ContainerId,
        expected_version: VersionNum,
    },
    Photo(Photo),
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum Outcome {
    Committed {
        commit: Commit,
    },
    AlreadyPresent {
        current: Container,
    },
    Conflict {
        current: Container,
    },
    #[serde(rename = "conflict")]
    RenameConflict {
        message: String,
    },
}

impl CommitFile {
    pub(super) fn run(self, progress: Option<TransferObserver>) -> Result<Value> {
        let remote = self.connection.open()?.with_progress(progress);
        let mut file = UploadFile::open(&self.file)?;
        let outcome = match self.target {
            Target::New { folder, name, tags } => match FileName::parse(name) {
                Ok(name) => create(&remote, &mut file, &NewContainer { folder, name, tags }),
                Err(_) => Ok(Outcome::RenameConflict {
                    message: "文件名无效，请改名另存。".into(),
                }),
            },
            Target::Version {
                container_id,
                expected_version,
            } => {
                let result = remote.append(&container_id, expected_version, &mut file);
                finish(
                    &remote,
                    &mut file,
                    result,
                    UploadLocation::Existing(&container_id),
                )
            }
            Target::Photo(photo) => photo.upload(&remote, &mut file),
        }?;
        Ok(serde_json::to_value(outcome)?)
    }
}

pub(super) fn create(
    remote: &Remote,
    file: &mut UploadFile,
    params: &NewContainer,
) -> Result<Outcome> {
    let result = remote.create(params, file);
    finish(remote, file, result, UploadLocation::New(params))
}

fn finish(
    remote: &Remote,
    file: &mut UploadFile,
    result: Result<Commit>,
    location: UploadLocation<'_>,
) -> Result<Outcome> {
    match result {
        Ok(commit) => Ok(Outcome::Committed { commit }),
        Err(error @ Error::Remote { status: 409, .. }) => {
            Ok(match remote.upload_conflict(location, file)? {
                Some(conflict) if conflict.already_present => Outcome::AlreadyPresent {
                    current: conflict.current,
                },
                Some(conflict) => Outcome::Conflict {
                    current: conflict.current,
                },
                None => Outcome::RenameConflict {
                    message: format!("{error}。请改名另存，或返回选择其他目录。"),
                },
            })
        }
        Err(error) => Err(error),
    }
}
