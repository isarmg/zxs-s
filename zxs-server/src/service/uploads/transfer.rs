use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use tokio::sync::mpsc::Receiver;

use crate::domain::{BUFFER, Blake3Hash, Error, Result, invalid};

pub(super) fn receive(
    chunks: Receiver<Vec<u8>>,
    path: &Path,
    size: u64,
    expected: Option<&Blake3Hash>,
) -> Result<Blake3Hash> {
    let file = output(path)?;
    let actual = consume(chunks, &file, size)?;
    if expected.is_some_and(|expected| expected != &actual) {
        return Err(invalid("payload BLAKE3 does not match headers"));
    }
    seal(&file)?;
    Ok(actual)
}

pub(super) fn consume(
    mut chunks: Receiver<Vec<u8>>,
    mut output: impl Write,
    size: u64,
) -> Result<Blake3Hash> {
    let mut hasher = blake3::Hasher::new();
    let mut remaining = size;
    while let Some(chunk) = chunks.blocking_recv() {
        remaining = remaining
            .checked_sub(chunk.len() as u64)
            .ok_or_else(|| invalid("payload exceeds Content-Length"))?;
        hasher.update_rayon(&chunk);
        output.write_all(&chunk)?;
    }
    if remaining != 0 {
        return Err(invalid("payload length does not match Content-Length"));
    }
    Blake3Hash::parse(hasher.finalize().to_hex().to_string())
}

pub(super) fn link(source: &super::Original, path: &Path) -> Result<bool> {
    if fs::hard_link(&source.path, path).is_err() {
        return Ok(false);
    }
    let linked = fs::symlink_metadata(path)?;
    let opened = source.file.metadata()?;
    if linked.ino() == opened.ino() && linked.dev() == opened.dev() {
        return Ok(true);
    }
    fs::remove_file(path)?;
    Ok(false)
}

pub(super) fn revert(source: &super::Original, path: &Path) -> Result<()> {
    let mut file = output(path)?;
    let (size, hash) = hash_file(&source.file, &mut file)?;
    if size != source.version.size
        || hash.finalize().to_hex().as_str() != source.version.blake3.as_str()
    {
        return Err(invalid(
            "stored original failed size or BLAKE3 verification",
        ));
    }
    Ok(seal(&file)?)
}

pub(super) fn import(input: &File, path: &Path) -> Result<(u64, Blake3Hash)> {
    let before = input.metadata()?;
    let mut file = output(path)?;
    let (size, hash) = hash_file(input, &mut file)?;
    let after = input.metadata()?;
    if size != before.len() || size != after.len() || before.modified()? != after.modified()? {
        return Err(Error::Conflict("source changed during import".into()));
    }
    seal(&file)?;
    Ok((
        size,
        Blake3Hash::parse(hash.finalize().to_hex().to_string())?,
    ))
}

fn output(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn seal(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o444))?;
    file.sync_all()
}
pub(in crate::service) fn hash_file(
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
