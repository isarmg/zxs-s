mod transfer;

use std::path::PathBuf;

use tokio::sync::mpsc::Receiver;

pub(super) use transfer::hash_file;

use super::{
    Blake3Hash, Change, Commit, Container, ContainerId, Error, File, NewContainer, NewVersion,
    Pending, Result, StageGuard, State, TxId, Version, VersionNum, invalid, new_container_id,
    new_tx_id, normalize_tags, now,
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

struct Original {
    file: File,
    path: PathBuf,
    version: Version,
}

enum Source {
    Body(Option<Blake3Hash>),
    Revert(Original),
}

pub struct Upload {
    tx_id: TxId,
    headers: UploadHeaders,
    container: Container,
    next: VersionNum,
    base: Base,
    source: Source,
    staging: StageGuard,
}

impl State {
    pub fn begin_upload(
        &self,
        intent: &UploadIntent,
        headers: UploadHeaders,
    ) -> Result<Box<Upload>> {
        if is_revert(intent) && (headers.size != 0 || headers.hash.is_some()) {
            return Err(invalid("revert requires an empty body without Zxs-Blake3"));
        }
        let (container, base, source) = match intent {
            UploadIntent::Create(params) => (
                self.prepare_new(params.clone())?,
                Base::Create,
                Source::Body(headers.hash.clone()),
            ),
            UploadIntent::Append(id, params) => self.prepare_append(id, params, &headers)?,
        };
        let next = match base {
            Base::Create => VersionNum::FIRST,
            Base::Append(_) => container.latest_version.next()?,
        };
        let tx_id = new_tx_id()?;
        let disk = self.store.paths.data_disk(&container.folder)?;
        let staging = self.store.stage_upload(&disk, &tx_id)?;
        Ok(Box::new(Upload {
            tx_id,
            headers,
            container,
            next,
            base,
            source,
            staging,
        }))
    }

    pub(super) fn import_payload(&self, input: &File, params: &NewContainer) -> Result<()> {
        let tx_id = new_tx_id()?;
        let container = self.prepare_new(params.clone())?;
        let disk = self.store.paths.data_disk(&container.folder)?;
        let staging = self.store.stage_upload(&disk, &tx_id)?;
        let (size, hash) = transfer::import(input, &staging.payload(&container.name))?;
        self.finish_upload(Upload {
            tx_id,
            headers: UploadHeaders { size, hash: None },
            container,
            next: VersionNum::FIRST,
            base: Base::Create,
            source: Source::Body(Some(hash)),
            staging,
        })?;
        Ok(())
    }

    fn prepare_new(&self, params: NewContainer) -> Result<Container> {
        self.read_publication(|| self.available(&params.folder, &params.name, None))?;
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

    fn prepare_append(
        &self,
        id: &ContainerId,
        params: &NewVersion,
        headers: &UploadHeaders,
    ) -> Result<(Container, Base, Source)> {
        let container = self.container(id)?;
        check_expected(container.latest_version, params.expected_version)?;
        let source = match params.revert_to_version {
            Some(version) => {
                let (version, file) = self.open_payload(id, version)?;
                let path = self.store.paths.payload(&container, &version);
                Source::Revert(Original {
                    file,
                    path,
                    version,
                })
            }
            None => Source::Body(headers.hash.clone()),
        };
        Ok((container, Base::Append(params.expected_version), source))
    }

    pub fn finish_upload(&self, mut upload: Upload) -> Result<Commit> {
        if let Source::Revert(original) = &upload.source {
            let path = upload.staging.payload(&upload.container.name);
            let linked = self.serialize_publication(|| {
                self.publication_target(&upload)?;
                if self
                    .db
                    .version(&upload.container.id, original.version.number)?
                    .is_none()
                {
                    return Err(Error::Missing("version not found"));
                }
                transfer::link(original, &path)
            })?;
            if !linked {
                transfer::revert(original, &path)?;
            }
        }
        let version = upload.metadata(now())?;
        upload.staging.save_version(&version)?;
        let (commit, pending) = self.serialize_publication(|| {
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
            self.transact(&pending, std::slice::from_mut(&mut upload.staging))?;
            Ok((commit, pending))
        })?;
        self.complete(&pending);
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
        let (size, hash) = match &self.source {
            Source::Body(hash) => (self.headers.size, hash.as_ref().ok_or(Error::Recovery)?),
            Source::Revert(original) => (original.version.size, &original.version.blake3),
        };
        Ok(Version {
            number: self.next,
            created_at: stamp,
            name_at_commit: self.container.name.clone(),
            size,
            blake3: hash.clone(),
        })
    }
}

impl Upload {
    pub async fn receive(mut self: Box<Self>, chunks: Receiver<Vec<u8>>) -> Result<Box<Self>> {
        tokio::task::spawn_blocking(move || {
            let path = self.staging.payload(&self.container.name);
            match &mut self.source {
                Source::Body(hash) => {
                    *hash = Some(transfer::receive(
                        chunks,
                        &path,
                        self.headers.size,
                        hash.as_ref(),
                    )?);
                }
                Source::Revert(_) => {
                    let mut chunks = chunks;
                    if chunks.blocking_recv().is_some() {
                        return Err(invalid("revert requires an empty body"));
                    }
                }
            }
            Ok(self)
        })
        .await?
    }
}

fn is_revert(intent: &UploadIntent) -> bool {
    matches!(intent, UploadIntent::Append(_, params) if params.revert_to_version.is_some())
}
