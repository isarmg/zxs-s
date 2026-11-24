use crate::domain::{OpId, Result, invalid};

/// # Errors
///
/// Returns an error if the system cannot provide random bytes.
pub fn random_hex<const N: usize>() -> Result<String> {
    use std::fmt::Write;
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| invalid(format!("system randomness: {error}")))?;
    let mut text = String::with_capacity(N * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    Ok(text)
}

/// # Errors
///
/// Returns an error if the system cannot generate an operation identifier.
pub fn new_op() -> Result<OpId> {
    OpId::parse(random_hex::<16>()?)
}
