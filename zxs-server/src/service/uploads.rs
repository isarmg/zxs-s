use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter};

use super::{
    Admission, BUFFER, Blake3Hash, Change, Commit, Container, ContainerId, Error, File, FileName,
    Lease, NewContainer, NewVersion, OpId, Pending, Receipt, Record, Result, StageGuard, State,
    Version, VersionNum, digest, fingerprint, invalid, new_container_id, normalize_tags, now,
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
        match &self.source {
            Source::Body { .. } => self.write_async(body).await,
            Source::Revert { file, .. } => {
                self.write_async(tokio::fs::File::from_std(file.try_clone()?))
                    .await
            }
        }
    }

    async fn write_async(&self, input: impl AsyncRead + Unpin) -> Result<()> {
        let (size, hash) = self.source.checks();
        let mut input = input.take(size + 1);
        let mut out = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.payload()?)
            .await?;
        let mut buffer = vec![0; BUFFER];
        let mut total = 0;
        let mut hasher = blake3::Hasher::new();
        let mut writer = BufWriter::with_capacity(BUFFER, &mut out);
        loop {
            let read = input.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            total += read as u64;
            hasher.update(&buffer[..read]);
            writer.write_all(&buffer[..read]).await?;
        }
        check_payload(total, size, &hasher, hash)?;
        writer.flush().await?;
        out.sync_all().await?;
        Ok(())
    }

    pub(super) fn copy_local(&self, reader: &mut File) -> Result<()> {
        let (size, hash) = self.source.checks();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.payload()?)?;
        let (total, hasher) = hash_file(reader.take(size + 1), &mut output)?;
        check_payload(total, size, &hasher, hash)?;
        output.sync_all()?;
        Ok(())
    }
}

pub(super) fn hash_file(
    mut input: impl Read,
    mut sink: impl Write,
) -> Result<(u64, blake3::Hasher)> {
    let mut buffer = vec![0; BUFFER];
    let mut total = 0;
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update_rayon(&buffer[..n]);
        sink.write_all(&buffer[..n])?;
    }
    Ok((total, hasher))
}

fn check_payload(
    total: u64,
    size: u64,
    hash: &blake3::Hasher,
    expected: &Blake3Hash,
) -> Result<()> {
    if total != size || hash.finalize().to_hex().as_str() != expected.as_str() {
        return Err(invalid(
            "payload length or BLAKE3 hash does not match headers",
        ));
    }
    Ok(())
}
