use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zxs_core::{
    Commit, Container, ContainerId, Error, FileName, FolderPath, NewContainer, Remote, Result,
    TransferObserver, UploadFile, VersionNum,
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
                let lookup = || remote.summary(&container_id).map(Some);
                match finish(&mut file, result, lookup) {
                    Err(Error::Remote { status: 404, .. }) => Ok(Outcome::RenameConflict {
                        message: "当前找不到原文件，请另存为新文件。".into(),
                    }),
                    result => result,
                }
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
    finish(file, result, || {
        remote.find_name(&params.folder, &params.name)
    })
}

fn finish(
    file: &mut UploadFile,
    result: Result<Commit>,
    lookup: impl FnOnce() -> Result<Option<Container>>,
) -> Result<Outcome> {
    match result {
        Ok(commit) => Ok(Outcome::Committed { commit }),
        Err(error @ Error::Remote { status: 409, .. }) => Ok(match lookup()? {
            Some(current) if file.matches_digest(&current.latest_blake3)? => {
                Outcome::AlreadyPresent { current }
            }
            Some(current) => Outcome::Conflict { current },
            None => Outcome::RenameConflict {
                message: format!("{error}。请改名另存，或返回选择其他目录。"),
            },
        }),
        Err(error) => Err(error),
    }
}
