use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
};

use super::{
    BUFFER, Blake3Hash, Body, Container, ContainerId, Method, Read, Remote, Response, Result,
    Version, VersionNum, invalid, limited, read_block, remote_error,
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
        if self.etag() != Some(hash.as_str()) {
            return Err(invalid("payload ETag does not match the selected version"));
        }
        Ok(())
    }

    fn etag(&self) -> Option<&str> {
        self.header("etag")?.strip_prefix('"')?.strip_suffix('"')
    }

    /// Returns the length and a reader for a complete payload verified against its `ETag`.
    ///
    /// # Errors
    /// Returns an error if the response is partial or its length or digest is invalid.
    pub fn into_verified_reader(self) -> Result<(u64, impl Read + Send + 'static)> {
        self.verified_reader(None)
    }

    fn verified_reader(
        self,
        selected: Option<&DownloadVersion>,
    ) -> Result<(u64, impl Read + Send + 'static)> {
        if self.status() != 200 {
            return Err(invalid("full payload download expected"));
        }
        let hash = if let Some(version) = selected {
            self.check_etag(&version.blake3)?;
            version.blake3.clone()
        } else {
            Blake3Hash::parse(
                self.etag()
                    .ok_or_else(|| invalid("payload ETag is missing"))?,
            )?
        };
        let size = self
            .header("content-length")
            .and_then(|size| size.parse::<u64>().ok())
            .filter(|&size| {
                selected
                    .and_then(|version| version.size)
                    .is_none_or(|n| n == size)
            })
            .ok_or_else(|| invalid("payload Content-Length does not match the selected version"))?;
        let hasher = blake3::Hasher::new();
        if size == 0 && hasher.finalize().to_hex().as_str() != hash.as_str() {
            return Err(invalid("download failed BLAKE3 verification"));
        }
        Ok((
            size,
            VerifiedPayload {
                reader: self.into_reader(),
                remaining: size,
                hasher,
                hash,
            },
        ))
    }
}

struct VerifiedPayload<R> {
    reader: R,
    remaining: u64,
    hasher: blake3::Hasher,
    hash: Blake3Hash,
}

impl<R: Read> Read for VerifiedPayload<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let limit = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(output.len());
        if limit == 0 {
            return Ok(0);
        }
        let count = read_block(&mut self.reader, &mut output[..limit])?;
        if count != limit {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "payload is truncated",
            ));
        }
        self.hasher.update_rayon(&output[..count]);
        self.remaining -= count as u64;
        // Withhold the final bytes until the whole payload has been verified.
        if self.remaining == 0 && self.hasher.finalize().to_hex().as_str() != self.hash.as_str() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "download failed BLAKE3 verification",
            ));
        }
        Ok(count)
    }
}

impl Remote {
    pub(crate) fn probe_bundle(
        &self,
        id: &ContainerId,
        version: VersionNum,
        hash: &Blake3Hash,
    ) -> Result<bool> {
        let response = self.payload(
            id,
            version,
            PayloadRequest {
                head: false,
                range: Some("bytes=0-39"),
            },
        )?;
        response.check_etag(hash)?;
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
        let (size, body) = self
            .payload(id, version.number, PayloadRequest::default())?
            .verified_reader(Some(version))?;
        let mut reader = self.track_download(body, size);
        let mut buffer = vec![0; BUFFER];
        loop {
            let n = match reader.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(invalid(error.to_string()));
                }
                result => result?,
            };
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
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
        self.detail(id)?.select_version(number)
    }
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
