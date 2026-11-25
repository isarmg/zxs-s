use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::domain::{BUFFER, Blake3Hash, Error, Result, invalid};

pub(super) async fn receive(
    input: impl AsyncRead + Unpin,
    path: PathBuf,
    size: u64,
    hash: &Blake3Hash,
) -> Result<()> {
    let mut input = input.take(size + 1);
    let mut writer = tokio::task::spawn_blocking(move || Writer::open(&path)).await??;
    let mut block = vec![0; BUFFER];
    loop {
        let filled = read_block(&mut input, &mut block).await?;
        if filled == 0 {
            break;
        }
        (writer, block) = tokio::task::spawn_blocking(move || -> Result<_> {
            writer.write(&block[..filled])?;
            Ok((writer, block))
        })
        .await??;
    }
    let expected = hash.clone();
    tokio::task::spawn_blocking(move || writer.finish(size, &expected)).await?
}

async fn read_block(input: &mut (impl AsyncRead + Unpin), block: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < block.len() {
        let read = input.read(&mut block[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

struct Writer {
    file: File,
    hasher: blake3::Hasher,
    length: u64,
}

impl Writer {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            file: output(path)?,
            hasher: blake3::Hasher::new(),
            length: 0,
        })
    }

    fn write(&mut self, buffer: &[u8]) -> Result<()> {
        self.hasher.update_rayon(buffer);
        self.file.write_all(buffer)?;
        self.length += buffer.len() as u64;
        Ok(())
    }

    fn finish(&self, size: u64, expected: &Blake3Hash) -> Result<()> {
        if self.length != size || self.hasher.finalize().to_hex().as_str() != expected.as_str() {
            return Err(invalid(
                "payload length or BLAKE3 hash does not match headers",
            ));
        }
        self.file.sync_all()?;
        Ok(())
    }
}

pub(super) fn import(input: &File, path: &Path) -> Result<(u64, Blake3Hash)> {
    let before = input.metadata()?;
    let mut file = output(path)?;
    let (size, hash) = hash_file(input, &mut file)?;
    let after = input.metadata()?;
    if size != before.len() || size != after.len() || before.modified()? != after.modified()? {
        return Err(Error::Conflict("source changed during import".into()));
    }
    file.sync_all()?;
    Ok((
        size,
        Blake3Hash::parse(hash.finalize().to_hex().to_string())?,
    ))
}

fn output(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
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
