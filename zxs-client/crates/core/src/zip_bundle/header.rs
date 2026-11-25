use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
};

use crate::{domain::Result, store::layout::IGNORE_FILE};

pub const LENGTH: usize = 40;

pub fn has_manifest(header: &[u8]) -> bool {
    header.len() >= LENGTH
        && header.starts_with(b"PK\x03\x04")
        && u16::from_le_bytes([header[26], header[27]]) as usize == IGNORE_FILE.len()
        && &header[30..LENGTH] == IGNORE_FILE.as_bytes()
}

pub fn local(path: &Path) -> Result<bool> {
    let mut header = [0; LENGTH];
    match File::open(path)?.read_exact(&mut header) {
        Ok(()) => Ok(has_manifest(&header)),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}
