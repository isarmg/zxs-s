use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};

use crate::domain::{BUFFER, Blake3Hash, Result};

pub fn file(path: &Path) -> Result<Blake3Hash> {
    copy(File::open(path)?, io::sink(), BUFFER).map(|(_, digest)| digest)
}

pub fn copy(
    mut input: impl Read,
    mut output: impl Write,
    size: usize,
) -> Result<(u64, Blake3Hash)> {
    let mut buffer = vec![0; size];
    let mut digest = blake3::Hasher::new();
    let mut total = 0;
    loop {
        let count = input.read(&mut buffer)?;
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
