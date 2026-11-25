use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

use super::{
    BUFFER, Body, Container, ContainerId, Error, Method, Read, Remote, Response, Result, Version,
    VersionNum, invalid, read_block, remote_error,
};

#[derive(Clone, Copy, Default)]
pub struct PayloadRequest<'a> {
    pub head: bool,
    pub range: Option<&'a str>,
}

pub struct Payload(Response<Body>);

impl Payload {
    #[must_use]
    pub fn status(&self) -> u16 {
        self.0.status().as_u16()
    }
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.0.headers().get(name)?.to_str().ok()
    }
    #[must_use]
    pub fn into_reader(self) -> impl Read + Send + 'static {
        self.0.into_body().into_reader()
    }
}

impl Remote {
    /// # Errors
    ///
    /// Returns an error if the remote has no authenticated session.
    pub fn media_url(&self, id: &ContainerId, version: VersionNum) -> Result<String> {
        Ok(format!(
            "{}{path}?token={token}",
            self.address,
            path = path(id, version),
            token = self.auth_token()?
        ))
    }

    /// # Errors
    ///
    /// Returns an error if the request fails or the server rejects access to the original.
    pub fn payload(
        &self,
        id: &ContainerId,
        version: VersionNum,
        options: PayloadRequest<'_>,
    ) -> Result<Payload> {
        self.auth_token()?;
        let method = if options.head {
            Method::HEAD
        } else {
            Method::GET
        };
        let mut request = self.request(method, &path(id, version));
        if let Some(value) = options.range {
            request = request.header("range", value);
        }
        let response = self.send(request, ())?;
        if matches!(response.status().as_u16(), 200 | 206 | 416) {
            Ok(Payload(response))
        } else {
            Err(remote_error(response))
        }
    }

    fn copy_payload(
        &self,
        id: &ContainerId,
        version: &Version,
        mut output: impl Write,
    ) -> Result<()> {
        let response = self.payload(id, version.version, PayloadRequest::default())?;
        if response.status() != 200 {
            return Err(invalid("full payload download expected"));
        }
        let mut reader = response.into_reader().take(version.size.saturating_add(1));
        let mut buffer = vec![0; BUFFER];
        let mut hasher = blake3::Hasher::new();
        let mut copied = 0;
        loop {
            let n = read_block(&mut reader, &mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update_rayon(&buffer[..n]);
            output.write_all(&buffer[..n])?;
            copied += n as u64;
        }
        if copied != version.size || hasher.finalize().to_hex().as_str() != version.blake3.as_str()
        {
            return Err(invalid("download failed size or BLAKE3 verification"));
        }
        output.flush()?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the destination exists, the stream fails, or the payload does not match its metadata.
    pub fn download_new(&self, id: &ContainerId, version: &Version, path: &Path) -> Result<()> {
        let file: File = OpenOptions::new().create_new(true).write(true).open(path)?;
        let result = self
            .copy_payload(id, version, &file)
            .and_then(|()| Ok(file.sync_all()?));
        drop(file);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }

    /// # Errors
    ///
    /// Returns an error if the container or requested version cannot be found or decoded.
    pub fn version(
        &self,
        id: &ContainerId,
        number: Option<VersionNum>,
    ) -> Result<(Container, Version)> {
        let detail = self.detail(id)?;
        let number = number.unwrap_or(detail.container.latest_version);
        let version = detail
            .versions
            .into_iter()
            .find(|v| v.version == number)
            .ok_or_else(|| Error::Remote {
                status: 404,
                message: format!("version {number} not found"),
            })?;
        Ok((detail.container, version))
    }
}

fn path(id: &ContainerId, version: VersionNum) -> String {
    format!("/containers/{id}/versions/{version}/payload")
}
