use std::{
    fs::{File, OpenOptions},
    io::Write,
    ops::Range,
    path::Path,
};

use super::{
    BUFFER, Blake3Hash, Body, Container, ContainerId, Error, Method, Read, Remote, Response,
    Result, Version, VersionNum, invalid, limited, read_block, remote_error,
};
use crate::zip_bundle::header::{LENGTH, has_manifest};

#[derive(Clone, Copy, Default)]
pub struct PayloadRequest<'a> {
    pub head: bool,
    pub range: Option<&'a str>,
}

pub struct Payload(Response<Body>);

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

    fn check_etag(&self, hash: &Blake3Hash) -> Result<()> {
        let found = self
            .header("etag")
            .and_then(|tag| tag.strip_prefix('"')?.strip_suffix('"'));
        if found != Some(hash.as_str()) {
            return Err(invalid("payload ETag does not match the selected version"));
        }
        Ok(())
    }
}

impl Remote {
    pub(crate) fn probe_bundle(
        &self,
        id: &ContainerId,
        version: VersionNum,
        hash: Option<&Blake3Hash>,
    ) -> Result<bool> {
        let response = self.payload(
            id,
            version,
            PayloadRequest {
                head: false,
                range: Some("bytes=0-39"),
            },
        )?;
        if let Some(hash) = hash {
            response.check_etag(hash)?;
        }
        if response.status() == 416 {
            return Ok(false);
        }
        let (start, end, total) = response
            .header("content-range")
            .and_then(byte_range)
            .filter(|_| response.status() == 206)
            .ok_or_else(|| invalid("expected a partial ZIP header response"))?;
        if start != 0 || end != total.min(LENGTH as u64) {
            return Err(invalid(
                "ZIP header response did not match the requested range",
            ));
        }
        let bytes = limited(response.into_reader(), LENGTH as u64)?;
        if bytes.len() as u64 != end {
            return Err(invalid("ZIP header response was truncated"));
        }
        Ok(has_manifest(&bytes))
    }

    pub(crate) fn payload_range(
        &self,
        id: &ContainerId,
        version: &Version,
        span: Range<u64>,
    ) -> Result<Payload> {
        let start = span.start;
        let end = span.end;
        let request = format!("bytes={start}-{}", end - 1);
        let response = self.payload(
            id,
            version.version,
            PayloadRequest {
                head: false,
                range: Some(&request),
            },
        )?;
        response.check_etag(&version.blake3)?;
        if response.status() != 206
            || response.header("content-range").and_then(byte_range)
                != Some((start, end, version.size))
        {
            return Err(invalid(
                "ZIP range response does not match the selected version",
            ));
        }
        Ok(response)
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
        mut output: impl Write,
    ) -> Result<()> {
        let response = self.payload(id, version.number, PayloadRequest::default())?;
        if response.status() != 200 {
            return Err(invalid("full payload download expected"));
        }
        response.check_etag(&version.blake3)?;
        let size = response
            .header("content-length")
            .and_then(|size| size.parse::<u64>().ok())
            .filter(|&size| version.size.is_none_or(|expected| size == expected))
            .ok_or_else(|| invalid("payload Content-Length does not match the selected version"))?;
        let mut reader = response.into_reader().take(size.saturating_add(1));
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
        if copied != size || hasher.finalize().to_hex().as_str() != version.blake3.as_str() {
            return Err(invalid("download failed size or BLAKE3 verification"));
        }
        output.flush()?;
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
        select_version(self.detail(id)?, number)
    }
}

fn select_version(
    detail: super::Detail,
    number: Option<VersionNum>,
) -> Result<(Container, Version)> {
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

fn path(id: &ContainerId, version: VersionNum) -> String {
    format!("/containers/{id}/versions/{version}/payload")
}

fn byte_range(value: &str) -> Option<(u64, u64, u64)> {
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, inclusive) = span.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = inclusive.parse::<u64>().ok()?.checked_add(1)?;
    (start < end).then_some((start, end, total.parse().ok()?))
}
