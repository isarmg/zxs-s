use std::{
    fs::File,
    io::{self, BufReader, Read, Seek},
    path::Path,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::json;

use super::{
    BUFFER, Blake3Hash, Body, Commit, Container, ContainerId, DeserializeOwned, Error, Method,
    NewContainer, NewVersion, Patch, RecycleId, Recycled, Remote, Response, Restore, Result,
    Session, TransferDirection, VersionNum, checked, decode, invalid, read_block,
};

pub struct UploadFile {
    pub size: u64,
    pub hash: Option<Blake3Hash>,
    pub(crate) streamed_hash: Option<Blake3Hash>,
    file: BufReader<File>,
}

impl UploadFile {
    /// # Errors
    /// Returns an error if the source file cannot be opened or measured.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            size,
            hash: None,
            streamed_hash: None,
            file: BufReader::with_capacity(BUFFER, file),
        })
    }

    /// Compares the upload bytes with an existing digest.
    ///
    /// # Errors
    /// Returns an error if the file changes or cannot be read.
    pub fn matches_digest(&mut self, expected: &Blake3Hash) -> Result<bool> {
        self.ensure_digest()?;
        Ok(self.hash.as_ref() == Some(expected))
    }

    pub(crate) fn ensure_digest(&mut self) -> Result<()> {
        if self.hash.is_some() {
            return Ok(());
        }
        if let Some(hash) = self.streamed_hash.take() {
            self.hash = Some(hash);
            return Ok(());
        }
        self.file.rewind()?;
        let before = self.file.get_ref().metadata()?;
        let mut input = (&mut self.file).take(self.size);
        let mut buffer = vec![0; BUFFER];
        let mut size = 0;
        let mut hasher = blake3::Hasher::new();
        loop {
            let count = read_block(&mut input, &mut buffer, || false)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            hasher.update_rayon(&buffer[..count]);
        }
        let after = self.file.get_ref().metadata()?;
        if size != self.size
            || after.len() != before.len()
            || after.modified()? != before.modified()?
        {
            return Err(Error::LocalConflict(
                "local file changed while calculating BLAKE3".into(),
            ));
        }
        self.hash = Some(Blake3Hash::parse(hasher.finalize().to_hex().to_string())?);
        Ok(())
    }
}

struct UploadReader<'f> {
    input: io::Take<&'f mut BufReader<File>>,
    hasher: blake3::Hasher,
}

impl Read for UploadReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(buffer)?;
        self.hasher.update(&buffer[..count]);
        Ok(count)
    }
}

impl Remote {
    /// # Errors
    ///
    /// Returns an error if authentication or the session response fails.
    pub fn login(&self, password: &str) -> Result<Session> {
        let request = self
            .request(Method::POST, "/sessions")
            .header("content-type", "application/json");
        let response = self.send(
            request,
            &serde_json::to_vec(&json!({"password": password}))?,
        )?;
        decode(checked(response).map_err(|error| match error {
            Error::Remote {
                status: 401,
                message,
            } => Error::PasswordRejected(message),
            error => error,
        })?)
    }

    /// # Errors
    /// Returns an error if the server cannot confirm session revocation.
    pub fn logout(&self) -> Result<()> {
        let Some(token) = self.token()? else {
            return Ok(());
        };
        let revoke = || {
            self.send(
                self.request(Method::DELETE, "/sessions/current")
                    .header("authorization", format!("Bearer {token}")),
                (),
            )
            .and_then(checked)
        };
        let reply = match revoke() {
            Err(Error::Http(_) | Error::Io(_)) => revoke(),
            result => result,
        };
        match reply {
            Ok(_) | Err(Error::Remote { status: 401, .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the server rejects the upload or the outcome cannot be confirmed.
    pub fn create(&self, params: &NewContainer, body: &mut UploadFile) -> Result<Commit> {
        self.upload("/containers", params, body)
    }

    /// # Errors
    ///
    /// Returns an error if upload fails, the expected version changes, or confirmation is unavailable.
    pub fn append(
        &self,
        id: &ContainerId,
        expected_version: VersionNum,
        body: &mut UploadFile,
    ) -> Result<Commit> {
        self.upload(
            &format!("/containers/{id}/versions"),
            &NewVersion { expected_version },
            body,
        )
    }

    /// # Errors
    ///
    /// Returns an error if the update is rejected or its outcome cannot be confirmed.
    pub fn patch(&self, id: &ContainerId, patch: &Patch) -> Result<Container> {
        self.change(Method::PATCH, &format!("/containers/{id}"), Some(patch))
    }

    /// # Errors
    ///
    /// Returns an error if the container cannot be recycled or the outcome is unknown.
    pub fn move_to_recycle(&self, id: &ContainerId) -> Result<Recycled> {
        self.recycle_operation(id, None)
    }

    pub(crate) fn recycle_operation(
        &self,
        id: &ContainerId,
        expected: Option<VersionNum>,
    ) -> Result<Recycled> {
        let mut path = format!("/containers/{id}");
        if let Some(expected) = expected {
            use std::fmt::Write;
            write!(path, "?expected_version={expected}")
                .map_err(|_| invalid("invalid recycle URL"))?;
        }
        self.change(Method::DELETE, &path, None::<&()>)
    }

    /// # Errors
    ///
    /// Returns an error if the recycle entry cannot be restored or the outcome is unknown.
    pub fn restore(&self, id: &RecycleId, restore: &Restore) -> Result<Container> {
        self.change(
            Method::POST,
            &format!("/recycle/{id}/restore"),
            Some(restore),
        )
    }

    fn change<T: DeserializeOwned, P: Serialize>(
        &self,
        method: Method,
        path: &str,
        value: Option<&P>,
    ) -> Result<T> {
        decode(self.send_change(method, path, value)?)
    }

    fn send_change<P: Serialize>(
        &self,
        method: Method,
        path: &str,
        value: Option<&P>,
    ) -> Result<Response<Body>> {
        let encoded = value.map(serde_json::to_vec).transpose()?;
        let result = self.authorized(method, path, |request| {
            if let Some(body) = &encoded {
                self.send(
                    request.header("content-type", "application/json"),
                    body.as_slice(),
                )
            } else {
                self.send(request, ())
            }
        });
        checked(result?)
    }

    pub(crate) fn create_folder(&self, folder: &super::Folder) -> Result<(super::Folder, bool)> {
        folder.folder.require_child()?;
        let response = self.send_change(Method::POST, "/folders", Some(folder))?;
        let created = response.status().as_u16() == 201;
        Ok((decode(response)?, created))
    }

    pub(crate) fn move_folder(&self, change: &super::FolderMove) -> Result<super::Folder> {
        self.change(Method::PATCH, "/folders", Some(change))
    }

    pub(crate) fn delete_folder(
        &self,
        folder: &super::FolderPath,
        recursive: bool,
    ) -> Result<super::Folder> {
        let path = super::folder_url(folder);
        let path = if recursive {
            format!("{path}&recursive=true")
        } else {
            path
        };
        self.change(Method::DELETE, &path, None::<&()>)
    }

    /// # Errors
    /// Returns an error if the recycle items cannot be removed or the result cannot be confirmed.
    pub fn purge(&self, id: Option<&RecycleId>) -> Result<crate::domain::Purged> {
        let path = id.map_or_else(|| "/recycle".into(), |id| format!("/recycle/{id}"));
        self.change(Method::DELETE, &path, None::<&()>)
    }

    fn upload(&self, path: &str, params: &impl Serialize, file: &mut UploadFile) -> Result<Commit> {
        let size = file.size;
        let params = URL_SAFE_NO_PAD.encode(serde_json::to_vec(params)?);
        let result = self.authorized(Method::POST, path, |request| {
            let request = request
                .header("zxs-params", &params)
                .header("content-length", size.to_string())
                .header("content-type", "application/octet-stream");
            self.send_file(request, file)
        });
        let commit: Commit = decode(checked(result?)?)?;
        if file.hash.as_ref().or(file.streamed_hash.as_ref()) != Some(&commit.version.blake3)
            || commit.version.size != size
        {
            return Err(invalid(
                "upload receipt did not match the bytes read from disk",
            ));
        }
        Ok(commit)
    }

    fn send_file(
        &self,
        mut request: super::Builder,
        source: &mut UploadFile,
    ) -> Result<Response<Body>> {
        source.file.rewind()?;
        if source.size > 0 {
            request = request.header("expect", "100-continue");
        }
        let input = (&mut source.file).take(source.size);
        if let Some(hash) = &source.hash {
            let request = request.header("zxs-blake3", hash.as_str());
            return self.send_upload(request, source.size, input);
        }
        let mut reader = UploadReader {
            input,
            hasher: blake3::Hasher::new(),
        };
        let result = self.send_upload(request, source.size, &mut reader);
        if reader.input.limit() == 0 {
            source.streamed_hash = Some(Blake3Hash::parse(
                reader.hasher.finalize().to_hex().to_string(),
            )?);
        }
        result
    }

    fn send_upload(
        &self,
        request: super::Builder,
        size: u64,
        input: impl Read,
    ) -> Result<Response<Body>> {
        let mut reader = self.track(input, TransferDirection::Upload, size);
        if size == 0 {
            self.send(request, &[][..])
        } else {
            self.send(request, ureq::SendBody::from_reader(&mut reader))
        }
    }
}
