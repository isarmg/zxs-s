use std::{fs::File, io, path::Path, sync::Arc};

use super::{
    BUFFER, Blake3Hash, Body, Container, ContainerId, Error, Method, Read, Remote, Response,
    Result, Version, VersionNum, invalid, progress::TransferDirection, remote_error,
};
use crate::store::digest;

#[derive(Clone, Copy, Default)]
pub struct PayloadRequest<'a> {
    pub head: bool,
    pub range: Option<&'a str>,
}

pub struct Payload(Response<Body>);

pub type DownloadCancellation = Arc<dyn Fn() -> bool + Send + Sync>;

pub(crate) struct DownloadVersion {
    pub number: VersionNum,
    pub blake3: Blake3Hash,
    pub size: Option<u64>,
}

impl From<&Version> for DownloadVersion {
    fn from(version: &Version) -> Self {
        Self {
            number: version.version,
            blake3: version.blake3.clone(),
            size: Some(version.size),
        }
    }
}

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

    fn download_size(&self, version: &DownloadVersion) -> Result<u64> {
        if self.status() != 200 {
            return Err(invalid("full payload download expected"));
        }
        let etag = self
            .header("etag")
            .and_then(|value| value.strip_prefix('"')?.strip_suffix('"'));
        if etag != Some(version.blake3.as_str()) {
            return Err(invalid("payload ETag does not match the selected version"));
        }
        self.header("content-length")
            .and_then(|size| size.parse::<u64>().ok())
            .filter(|&size| version.size.is_none_or(|n| n == size))
            .ok_or_else(|| invalid("payload Content-Length does not match the selected version"))
    }
}

impl Remote {
    /// Checks for cancellation before requesting a download and between body reads.
    #[must_use]
    pub fn with_download_cancellation(mut self, cancelled: Option<DownloadCancellation>) -> Self {
        self.download_cancelled = cancelled;
        self
    }

    fn check_download_cancelled(&self) -> Result<()> {
        if self
            .download_cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled())
        {
            return Err(Error::DownloadCancelled);
        }
        Ok(())
    }

    /// # Errors
    /// Returns an error if the client cannot obtain a session for playback.
    pub fn media_request(&self, id: &ContainerId, version: VersionNum) -> Result<(String, String)> {
        Ok((
            format!("{}{}", self.endpoint(), path(id, version)),
            format!("Bearer {}", self.current_token(None)?),
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
        let method = if options.head {
            Method::HEAD
        } else {
            Method::GET
        };
        let response = self.authorized(method, &path(id, version), |request| {
            let request = if let Some(range) = options.range {
                request.header("range", range)
            } else {
                request
            };
            self.send(request, ())
        })?;
        if matches!(response.status().as_u16(), 200 | 206 | 416) {
            Ok(Payload(response))
        } else {
            Err(remote_error(response))
        }
    }

    fn copy_payload(
        &self,
        id: &ContainerId,
        version: &DownloadVersion,
        output: &File,
    ) -> Result<()> {
        self.check_download_cancelled()?;
        let payload = self.payload(id, version.number, PayloadRequest::default())?;
        let size = payload.download_size(version)?;
        let mut reader = self.track(
            payload.into_reader().take(size),
            TransferDirection::Download,
            size,
        );
        let (length, hash) = digest::copy_cancellable(&mut reader, output, BUFFER, || {
            self.check_download_cancelled().is_err()
        })?;
        if size != length {
            return Err(
                io::Error::new(io::ErrorKind::UnexpectedEof, "payload is truncated").into(),
            );
        }
        if hash != version.blake3 {
            return Err(invalid("download failed BLAKE3 verification"));
        }
        self.check_download_cancelled()?;
        reader.verified();
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the destination exists, the stream fails, or the payload does not match its metadata.
    pub fn download_new(&self, id: &ContainerId, version: &Version, path: &Path) -> Result<()> {
        self.download_version(id, &version.into(), path)
    }

    pub(crate) fn download_version(
        &self,
        id: &ContainerId,
        version: &DownloadVersion,
        path: &Path,
    ) -> Result<()> {
        let file = File::create_new(path)?;
        let result = self
            .copy_payload(id, version, &file)
            .and_then(|()| Ok(file.sync_all()?));
        let result = self.check_download_cancelled().and(result);
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
        self.detail(id)?.select_version(number)
    }
}

fn path(id: &ContainerId, version: VersionNum) -> String {
    format!("/containers/{id}/versions/{version}/payload")
}
