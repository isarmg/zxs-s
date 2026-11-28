mod transfer;

use tokio::sync::mpsc::Receiver;

use super::{
    Blake3Hash, Change, Commit, Container, ContainerId, Error, File, NewContainer, NewVersion,
    Pending, Result, StageGuard, State, Store, TxId, Version, VersionNum, new_container_id,
    new_tx_id, normalize_tags, now, publication::Prepared, writes::WritePermit,
};

pub enum UploadIntent {
    Create(NewContainer),
    Append(ContainerId, NewVersion),
}

pub struct UploadHeaders {
    pub size: u64,
    pub hash: Option<Blake3Hash>,
}

enum Base {
    Create,
    Append(VersionNum),
}

pub struct Upload {
    tx_id: TxId,
    headers: UploadHeaders,
    container: Container,
    next: VersionNum,
    base: Base,
    staging: StageGuard,
    _write: WritePermit,
}

impl State {
    pub fn begin_upload(
        &self,
        intent: &UploadIntent,
        headers: UploadHeaders,
    ) -> Result<Box<Upload>> {
        let tx_id = new_tx_id()?;
        let (container, base, write) = self.read_publication(|| {
            let (container, base, expected_path) = match intent {
                UploadIntent::Create(params) => {
                    (self.prepare_new(params.clone())?, Base::Create, None)
                }
                UploadIntent::Append(id, params) => {
                    let container = self.container(id)?;
                    check_expected(container.latest_version, params.expected_version)?;
                    (
                        container,
                        Base::Append(params.expected_version),
                        params.expected_path.as_ref(),
                    )
                }
            };
            let path = container.folder.child(container.name.as_str())?;
            if let Some(expected) = expected_path
                && expected != &path
            {
                return Err(Error::Conflict(format!(
                    "container moved or was renamed before upload from {expected} to {path}"
                )));
            }
            let write = self.writes.admit(&tx_id, &[&path])?;
            Ok((container, base, write))
        })?;
        let next = match base {
            Base::Create => VersionNum::FIRST,
            Base::Append(_) => container.latest_version.next()?,
        };
        let disk = self.store.paths.data_disk(&container.folder)?;
        let staging = self.store.stage_upload(&disk, &tx_id)?;
        Ok(Box::new(Upload {
            tx_id,
            headers,
            container,
            next,
            base,
            staging,
            _write: write,
        }))
    }

    pub(super) fn import_payload(&self, input: &File, params: &NewContainer) -> Result<()> {
        let intent = UploadIntent::Create(params.clone());
        let before = input.metadata()?;
        let mut upload = self.begin_upload(
            &intent,
            UploadHeaders {
                size: before.len(),
                hash: None,
            },
        )?;
        upload.headers.hash = Some(transfer::import(
            input,
            &before,
            &upload.staging.payload(&upload.container.name),
        )?);
        self.finish_upload(*upload)?;
        Ok(())
    }

    fn prepare_new(&self, params: NewContainer) -> Result<Container> {
        self.available(&params.folder, &params.name, None)?;
        let created_at = now();
        Ok(Container {
            id: new_container_id()?,
            folder: params.folder,
            name: params.name,
            tags: normalize_tags(params.tags),
            latest_version: VersionNum::FIRST,
            latest_blake3: None,
            created_at,
            updated_at: created_at,
        })
    }

    pub fn finish_upload(&self, mut upload: Upload) -> Result<Commit> {
        let version = upload.metadata(now())?;
        upload.staging.save_version(&version)?;
        let (commit, cleanup) = self.serialize_publication(|| {
            let mut container = self.publication_target(&upload)?;
            container.latest_version = upload.next;
            container.latest_blake3 = Some(version.blake3.clone());
            container.updated_at = now();
            let commit = Commit { container, version };
            let pending = Pending {
                tx_id: upload.tx_id,
                change: Change::Publish {
                    commit: Box::new(commit.clone()),
                },
            };
            let cleanup = self.transact(
                &pending,
                std::slice::from_mut(&mut upload.staging),
                Prepared::None,
            )?;
            Ok((commit, cleanup))
        })?;
        Store::finish(cleanup);
        Ok(commit)
    }

    fn publication_target(&self, upload: &Upload) -> Result<Container> {
        match upload.base {
            Base::Create => {
                self.available(&upload.container.folder, &upload.container.name, None)?;
                Ok(upload.container.clone())
            }
            Base::Append(expected) => {
                let current = self.container(&upload.container.id)?;
                check_expected(current.latest_version, expected)?;
                if current.name != upload.container.name
                    || current.folder != upload.container.folder
                {
                    return Err(Error::Conflict(
                        "container moved or was renamed during upload".into(),
                    ));
                }
                Ok(current)
            }
        }
    }
}

pub(super) fn check_expected(latest: VersionNum, expected: VersionNum) -> Result<()> {
    if latest != expected {
        return Err(Error::Conflict(format!(
            "expected version {expected}, latest is {latest}"
        )));
    }
    Ok(())
}

impl Upload {
    fn metadata(&self, stamp: u64) -> Result<Version> {
        Ok(Version {
            number: self.next,
            created_at: stamp,
            name_at_commit: self.container.name.clone(),
            size: self.headers.size,
            blake3: self.headers.hash.clone().ok_or(Error::Recovery)?,
        })
    }
}

impl Upload {
    pub async fn receive(mut self: Box<Self>, chunks: Receiver<Vec<u8>>) -> Result<Box<Self>> {
        tokio::task::spawn_blocking(move || {
            let path = self.staging.payload(&self.container.name);
            self.headers.hash = Some(transfer::receive(
                chunks,
                &path,
                self.headers.size,
                self.headers.hash.as_ref(),
            )?);
            Ok(self)
        })
        .await?
    }
}
