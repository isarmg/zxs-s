use crate::store::layout::IGNORE_FILE;

pub const LENGTH: usize = 40;

pub fn has_manifest(header: &[u8]) -> bool {
    header.len() >= LENGTH
        && header.starts_with(b"PK\x03\x04")
        && u16::from_le_bytes([header[26], header[27]]) as usize == IGNORE_FILE.len()
        && &header[30..LENGTH] == IGNORE_FILE.as_bytes()
}
