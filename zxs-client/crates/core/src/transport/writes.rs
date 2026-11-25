use std::{
    fs::File,
    io::{BufReader, Seek},
    path::Path,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::json;

use super::{
    BUFFER, Blake3Hash, Body, Commit, Container, ContainerId, DeserializeOwned, Error, Method,
    NewContainer, NewVersion, OpId, Patch, RecycleId, Recycled, Remote, Response, Restore, Result,
    Session, VersionNum, checked, decode, new_op, read_block,
};

pub struct HashedFile {
    pub size: u64,
    pub hash: Blake3Hash,
    file: BufReader<File>,
}

impl HashedFile {
    /// # Errors
    ///
    /// Returns an error if the source cannot be opened, read, or rewound.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut buffer = vec![0; BUFFER];
        let mut size = 0;
        let mut hasher = blake3::Hasher::new();
        loop {
            let n = read_block(&mut file, &mut buffer)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            hasher.update_rayon(&buffer[..n]);
        }
        file.rewind()?;
        Ok(Self {
            size,
            hash: Blake3Hash::parse(hasher.finalize().to_hex().to_string())?,
            file: BufReader::with_capacity(BUFFER, file),
        })
    }
}

impl Remote {
    /// # Errors
    ///
    /// Returns an error if authentication or the session response fails.
    pub fn login(&self, password: &str) -> Result<Session> {
        let op = new_op()?;
        let login = || {
            let request = self
                .request(Method::POST, "/sessions")
                .header("zxs-op-id", op.as_str())
                .header("content-type", "application/json");
            self.send(
                request,
                &serde_json::to_vec(&json!({ "password": password }))?,
            )
            .and_then(checked)
            .and_then(decode)
        };
        match login() {
            Err(Error::Remote { status, message }) if status < 500 => {
                Err(Error::Remote { status, message })
            }
            Err(_) => login(),
            success => success,
        }
    }

    /// # Errors
    ///
    /// Returns an error if the server cannot confirm session revocation.
    pub fn logout(&self) -> Result<()> {
        self.auth_token()?;
        let op = new_op()?;
        let logout = || {
            self.send(
                self.request(Method::DELETE, "/sessions/current")
                    .header("zxs-op-id", op.as_str()),
                (),
            )
            .and_then(checked)
            .map(|_| ())
        };
        match logout() {
            Err(Error::Http(_) | Error::Io(_)) => logout(),
            result => result,
        }
    }

    /// # Errors
    ///
    /// Returns an error if the server rejects the upload or the outcome cannot be confirmed.
    pub fn create(&self, op: OpId, params: &NewContainer, body: HashedFile) -> Result<Commit> {
        self.upload(op, "/containers", params, UploadBody::File(body))
    }

    /// # Errors
    ///
    /// Returns an error if upload fails, the expected version changes, or confirmation is unavailable.
    pub fn append(
        &self,
        op: OpId,
        id: &ContainerId,
        expected_version: VersionNum,
        body: HashedFile,
    ) -> Result<Commit> {
        self.upload(
            op,
            &format!("/containers/{id}/versions"),
            &NewVersion {
                expected_version,
                revert_to_version: None,
            },
            UploadBody::File(body),
        )
    }

    /// # Errors
    ///
    /// Returns an error if the version is missing, the expected version changes, or publication fails.
    pub fn revert(
        &self,
        id: &ContainerId,
        expected_version: VersionNum,
        version: VersionNum,
    ) -> Result<Commit> {
        self.upload(
            new_op()?,
            &format!("/containers/{id}/versions"),
            &NewVersion {
                expected_version,
                revert_to_version: Some(version),
            },
            UploadBody::Empty,
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
        self.change(Method::DELETE, &format!("/containers/{id}"), None::<&()>)
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
        self.auth_token()?;
        let op = new_op()?;
        let request = self.request(method, path).header("zxs-op-id", op.as_str());
        let result = match value {
            Some(value) => self.send(
                request.header("content-type", "application/json"),
                serde_json::to_vec(value)?,
            ),
            None => self.send(request, ()),
        };
        self.confirm(result, op)
    }

    fn upload(
        &self,
        op: OpId,
        path: &str,
        params: &impl Serialize,
        mut body: UploadBody,
    ) -> Result<Commit> {
        self.auth_token()?;
        let (size, hash) = match &body {
            UploadBody::File(file) => (file.size, file.hash.to_string()),
            UploadBody::Empty => (0, blake3::hash(&[]).to_hex().to_string()),
        };
        let params = URL_SAFE_NO_PAD.encode(serde_json::to_vec(params)?);
        let request = self
            .request(Method::POST, path)
            .header("zxs-op-id", op.as_str())
            .header("zxs-blake3", hash)
            .header("zxs-params", params)
            .header("content-length", size.to_string())
            .header("content-type", "application/octet-stream");
        let result = match &mut body {
            UploadBody::File(file) if size > 0 => self.send(
                request.header("expect", "100-continue"),
                ureq::SendBody::from_reader(&mut file.file),
            ),
            _ => self.send(request, &[][..]),
        };
        self.confirm(result, op)
    }

    fn confirm<T: DeserializeOwned>(
        &self,
        response: Result<Response<Body>>,
        op: OpId,
    ) -> Result<T> {
        let error = match response.and_then(checked).and_then(decode) {
            Err(Error::Remote { status, message }) if status < 500 => {
                return Err(Error::Remote { status, message });
            }
            Err(error) => error,
            success => return success,
        };
        if let Ok(receipt) = self.receipt(&op) {
            return Ok(serde_json::from_value(receipt.result)?);
        }
        Err(Error::Uncertain {
            op,
            source: Box::new(error),
        })
    }
}

enum UploadBody {
    File(HashedFile),
    Empty,
}
