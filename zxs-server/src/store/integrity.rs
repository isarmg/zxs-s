use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};

use crate::domain::{
    BUFFER, ContainerMeta, Detail, DiskName, Error, Recycled, Result, Version, invalid,
    normalize_tags,
};

use super::{
    Layout, Store,
    fs_ops::{container_meta, read_version_files},
};

impl Store {
    pub fn verify_versions(path: &Path) -> Result<usize> {
        Ok(verified_versions(path, &container_meta(path)?)?.len())
    }

    pub fn verify_archive(&self, disk: &DiskName, batch: &Path) -> Result<usize> {
        let (entry, meta) = self.archive_entry(disk, batch)?;
        let path = Layout::recycled_container(batch, &entry.container.name);
        Ok(verified_versions(&path, &meta)?.len())
    }

    pub fn archive_snapshot(&self, disk: &DiskName, batch: &Path) -> Result<Recycled> {
        let (mut entry, meta) = self.archive_entry(disk, batch)?;
        let path = Layout::recycled_container(batch, &entry.container.name);
        let versions = read_versions(&path, &meta)?;
        entry.container.tags = normalize_tags(entry.container.tags);
        entry.container.latest_blake3 = versions.last().map(|version| version.blake3.clone());
        Ok(entry)
    }

    fn archive_entry(&self, disk: &DiskName, batch: &Path) -> Result<(Recycled, ContainerMeta)> {
        let (entry, meta) = Self::read_recycled(disk, batch)
            .map_err(|error| invalid(format!("{}: {error}", batch.display())))?;
        if self.paths.recycled_entry(&entry) != batch {
            return Err(Error::IncompleteHistory(batch.into()));
        }
        Ok((entry, meta))
    }

    pub fn check_history(path: &Path, expected: &Detail) -> Result<()> {
        let meta = container_meta(path)?;
        if meta.id != expected.container.id || read_versions(path, &meta)? != expected.versions {
            return Err(Error::IncompleteHistory(path.into()));
        }
        verify_payloads(path, &expected.versions)
    }

    pub fn check_indexed_paths(&self) -> Result<()> {
        super::db::check_indexed_paths(&self.paths)
    }
}

pub(super) fn read_versions(path: &Path, meta: &ContainerMeta) -> Result<Vec<Version>> {
    let versions = read_version_files(path)?;
    let first = meta.first_retained_version;
    let latest = meta.latest_version;
    if versions.first().map(|v| v.number) != Some(first)
        || versions.last().map(|v| v.number) != Some(latest)
        || versions.len() != (latest.get() - first.get() + 1) as usize
    {
        return Err(Error::IncompleteHistory(path.into()));
    }
    Ok(versions)
}

pub(super) fn verified_versions(path: &Path, meta: &ContainerMeta) -> Result<Vec<Version>> {
    let versions = read_versions(path, meta)?;
    verify_payloads(path, &versions)?;
    Ok(versions)
}

fn verify_payloads(path: &Path, versions: &[Version]) -> Result<()> {
    for version in versions {
        let directory = Layout::version(path, version.number);
        let payload = Layout::payload_file(&directory, &version.name_at_commit);
        let (size, hash) = hash_file(File::open(&payload)?, io::sink(), version.size)?;
        if size != version.size || hash.finalize().to_hex().as_str() != version.blake3.as_str() {
            return Err(Error::CorruptPayload(payload));
        }
    }
    Ok(())
}

pub fn hash_file(
    mut input: impl Read,
    mut sink: impl Write,
    size: u64,
) -> Result<(u64, blake3::Hasher)> {
    let capacity = usize::try_from(size).unwrap_or(BUFFER).clamp(8192, BUFFER);
    let mut buffer = vec![0; capacity];
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
