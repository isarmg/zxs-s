mod transfer;

use std::path::PathBuf;

use serde::Serialize;
use tokio::io::AsyncRead;

pub(super) use transfer::hash_file;

use super::{
    Admission, Blake3Hash, Change, Commit, Container, ContainerId, Error, File, FileName, Lease,
    NewContainer, NewVersion, OpId, Pending, Receipt, Record, Result, StageGuard, State, Version,
    VersionNum, digest, fingerprint, hex, invalid, new_container_id, normalize_tags, now, random,
};

#[derive(Serialize)]
pub enum UploadIntent {
    Create(NewContainer),
    Append(ContainerId, NewVersion),
}

pub enum BeginUpload {
    Replay(Receipt),
    Ready(Box<Upload>),
}

enum Base {
    Create,
    Append(VersionNum),
}

enum Source {
    Body { size: u64, hash: Blake3Hash },
    Revert { file: File, version: Version },
}

pub struct Upload {
    pub(super) _lease: Lease,
    op: OpId,
    fingerprint: Blake3Hash,
    container: Container,
    next: VersionNum,
    base: Base,
    source: Source,
    staging: StageGuard,
}

impl Source {
    fn checks(&self) -> (u64, &Blake3Hash) {
        match self {
            Self::Body { size, hash } => (*size, hash),
            Self::Revert { version, .. } => (version.size, &version.blake3),
        }
    }
}

impl State {
    pub fn begin_upload(
        &self,
        op: OpId,
        intent: UploadIntent,
        size: u64,
        hash: Blake3Hash,
    ) -> Result<BeginUpload> {
        if size > i64::MAX as u64 {
            return Err(invalid("payload length is too large"));
        }
        let fingerprint = fingerprint(&("upload", &intent, size, &hash))?;
        let lease = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return Ok(BeginUpload::Replay(receipt)),
            Admission::New(lease) => lease,
        };
        let (container, base, source) = match intent {
            UploadIntent::Create(params) => (
                self.prepare_new(params)?,
                Base::Create,
                Source::Body { size, hash },
            ),
            UploadIntent::Append(id, params) => self.prepare_append(&id, &params, size, hash)?,
        };
        let next = match base {
            Base::Create => VersionNum::FIRST,
            Base::Append(v) => v.next()?,
        };
        let staging = self.with_container(&container.id, || {
            if let Base::Append(expected) = base {
                check_expected(self.container(&container.id)?.latest_version, expected)?;
            }
            self.store.stage(&container.id, &op)
        })?;
        Ok(BeginUpload::Ready(Box::new(Upload {
            _lease: lease,
            op,
            fingerprint,
            container,
            next,
            base,
            source,
            staging,
        })))
    }

    pub(super) fn import_payload(&self, input: &File, params: &NewContainer) -> Result<()> {
        let op = OpId::parse(hex(random()?))?;
        let container = self.prepare_new(params.clone())?;
        let staging =
            self.with_container(&container.id, || self.store.stage(&container.id, &op))?;
        let path = staging.payload(&FileName::parse(container.name.payload_file())?);
        let (size, hash) = transfer::import(input, &path)?;
        let intent = UploadIntent::Create(params.clone());
        let fingerprint = fingerprint(&("upload", intent, size, &hash))?;
        let Admission::New(lease) = self.admit(&op, &fingerprint)? else {
            return Err(Error::Recovery);
        };
        self.finish_upload(Upload {
            _lease: lease,
            op,
            fingerprint,
            container,
            next: VersionNum::FIRST,
            base: Base::Create,
            source: Source::Body { size, hash },
            staging,
        })?;
        Ok(())
    }

    fn prepare_new(&self, params: NewContainer) -> Result<Container> {
        self.available(&params.folder, &params.name, None)?;
        let created_at = now();
        Ok(Container {
            id: new_container_id()?,
            folder: params.folder,
            name: params.name,
            tags: normalize_tags(params.tags)?,
            latest_version: VersionNum::FIRST,
            created_at,
            updated_at: created_at,
        })
    }

    fn prepare_append(
        &self,
        id: &ContainerId,
        params: &NewVersion,
        size: u64,
        hash: Blake3Hash,
    ) -> Result<(Container, Base, Source)> {
        let container = self.container(id)?;
        check_expected(container.latest_version, params.expected_version)?;
        let source = match params.revert_to_version {
            Some(version) => {
                if size != 0 || hash != digest(&[])? {
                    return Err(invalid("revert requires an empty body and its BLAKE3 hash"));
                }
                let (version, file) = self.open_payload(id, version)?;
                Source::Revert { file, version }
            }
            None => Source::Body { size, hash },
        };
        Ok((container, Base::Append(params.expected_version), source))
    }

    pub fn finish_upload(&self, mut upload: Upload) -> Result<Receipt> {
        let id = upload.container.id.clone();
        self.mutate(&id, || {
            let mut container = match upload.base {
                Base::Create => {
                    self.available(&upload.container.folder, &upload.container.name, None)?;
                    upload.container.clone()
                }
                Base::Append(expected) => {
                    let current = self.container(&id)?;
                    check_expected(current.latest_version, expected)?;
                    if current.name != upload.container.name {
                        return Err(Error::Conflict(
                            "container name changed during upload".into(),
                        ));
                    }
                    current
                }
            };
            let stamp = now();
            container.latest_version = upload.next;
            container.updated_at = stamp;
            let version = upload.metadata(stamp)?;
            upload.staging.save_version(&version)?;
            let commit = Commit { container, version };
            let record = Record::new(upload.op.clone(), upload.fingerprint.clone(), 201, &commit)?;
            let pending = Pending {
                change: Change::Publish {
                    commit: Box::new(commit),
                },
                record: Some(record.clone()),
            };
            self.transact(&pending, Some(&mut upload.staging))?;
            Ok(record.receipt)
        })
    }
}

fn check_expected(latest: VersionNum, expected: VersionNum) -> Result<()> {
    if latest != expected {
        return Err(Error::Conflict(format!(
            "expected version {expected}, latest is {latest}"
        )));
    }
    Ok(())
}

impl Upload {
    fn payload(&self) -> Result<PathBuf> {
        Ok(self
            .staging
            .payload(&FileName::parse(self.container.name.payload_file())?))
    }

    fn metadata(&self, stamp: u64) -> Result<Version> {
        let (size, hash) = self.source.checks();
        Ok(Version {
            number: self.next,
            op_id: self.op.clone(),
            created_at: stamp,
            name_at_commit: self.container.name.clone(),
            size,
            blake3: hash.clone(),
            payload_file: FileName::parse(self.container.name.payload_file())?,
        })
    }

    pub async fn receive(&self, body: impl AsyncRead + Unpin) -> Result<()> {
        let (size, hash) = self.source.checks();
        let path = self.payload()?;
        match &self.source {
            Source::Body { .. } => transfer::receive(body, path, size, hash).await,
            Source::Revert { file, .. } => {
                let reader = tokio::fs::File::from_std(file.try_clone()?);
                transfer::receive(reader, path, size, hash).await
            }
        }
    }
}
