#[cfg(test)]
mod tests;

use super::{Error, Result, SessionToken, State, random};
use crate::domain::Session;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};

pub fn password_hash(password: &str) -> Result<String> {
    let salt = SaltString::encode_b64(&random::<16>()?).map_err(config_error)?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(config_error)?
        .to_string())
}

fn config_error(error: impl std::fmt::Display) -> Error {
    Error::Config(error.to_string())
}

impl State {
    pub fn login(&self, password: &str) -> Result<Session> {
        self.healthy()?;
        let _guard = self.password_check.lock().map_err(|_| Error::Recovery)?;
        let stored = PasswordHash::new(&self.config.password_hash).map_err(config_error)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &stored)
            .map_err(|_| Error::Unauthorized)?;
        let token = SessionToken::parse(
            blake3::Hash::from_bytes(random::<32>()?)
                .to_hex()
                .to_string(),
        )?;
        self.sessions
            .lock()
            .map_err(|_| Error::Recovery)?
            .insert(blake3::hash(token.as_str().as_bytes()));
        Ok(Session { token })
    }

    pub fn logout(&self, token: &SessionToken) -> Result<()> {
        self.healthy()?;
        if self
            .sessions
            .lock()
            .map_err(|_| Error::Recovery)?
            .remove(&blake3::hash(token.as_str().as_bytes()))
        {
            return Ok(());
        }
        Err(Error::Unauthorized)
    }

    pub fn authorize(&self, token: &SessionToken) -> Result<()> {
        self.healthy()?;
        if self
            .sessions
            .lock()
            .map_err(|_| Error::Recovery)?
            .contains(&blake3::hash(token.as_str().as_bytes()))
        {
            return Ok(());
        }
        Err(Error::Unauthorized)
    }
}
