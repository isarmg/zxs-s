use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};

use crate::domain::{BUFFER, Blake3Hash, Error, Result};

/// Computes BLAKE3 without loading the entire file into memory.
///
/// # Errors
/// Returns an error if the file cannot be opened or read.
pub fn file(path: &Path) -> Result<Blake3Hash> {
    let file = File::open(path)?;
    copy(&file, io::sink(), buffer_size(file.metadata()?.len())).map(|(_, digest)| digest)
}

pub(crate) fn buffer_size(size: u64) -> usize {
    usize::try_from(size).unwrap_or(BUFFER).clamp(8192, BUFFER)
}

pub fn copy(input: impl Read, output: impl Write, size: usize) -> Result<(u64, Blake3Hash)> {
    copy_cancellable(input, output, size, || false)
}

pub fn copy_cancellable(
    mut input: impl Read,
    mut output: impl Write,
    size: usize,
    cancelled: impl Fn() -> bool,
) -> Result<(u64, Blake3Hash)> {
    let mut buffer = vec![0; size];
    let mut digest = blake3::Hasher::new();
    let mut total = 0;
    loop {
        let count = read_block(&mut input, &mut buffer, &cancelled)?;
        if count == 0 {
            break;
        }
        digest.update_rayon(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        total += count as u64;
    }
    Ok((
        total,
        Blake3Hash::parse(digest.finalize().to_hex().to_string())?,
    ))
}

fn read_block(
    input: &mut impl Read,
    buffer: &mut [u8],
    cancelled: impl Fn() -> bool,
) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        if cancelled() {
            return Err(Error::DownloadCancelled);
        }
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::{BUFFER, buffer_size};

    #[test]
    fn read_buffer_keeps_a_block_for_empty_size_hints() {
        assert_eq!(buffer_size(0), 8192);
        assert_eq!(buffer_size(1), 8192);
        assert_eq!(buffer_size(u64::MAX), BUFFER);
    }
}
