use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use serde_json::json;
use subtle::ConstantTimeEq;

use super::{
    Admission, Blake3Hash, Error, OpId, Receipt, Record, Result, SessionEffect, SessionToken,
    State, db, digest, fingerprint, invalid, now, random,
};

const LIFETIME: u64 = 30 * 24 * 60 * 60;

pub fn password_hash(password: &str) -> Result<String> {
    check_password(password)?;
    let salt = SaltString::encode_b64(&random::<16>()?).map_err(config_error)?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(config_error)?
        .to_string())
}

pub fn session_key() -> Result<Blake3Hash> {
    Blake3Hash::parse(
        blake3::Hash::from_bytes(random::<32>()?)
            .to_hex()
            .to_string(),
    )
}

fn check_password(password: &str) -> Result<()> {
    if password.is_empty() || password.len() > 1024 {
        Err(invalid("password must contain 1 to 1024 UTF-8 bytes"))
    } else {
        Ok(())
    }
}

fn config_error(error: impl std::fmt::Display) -> Error {
    Error::Config(error.to_string())
}

impl State {
    pub fn login(&self, op: OpId, password: &str) -> Result<Receipt> {
        self.healthy()?;
        check_password(password)?;
        let _guard = self.password_check.lock().map_err(|_| Error::Recovery)?;
        let stored = PasswordHash::new(&self.config.password_hash).map_err(config_error)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &stored)
            .map_err(|_| Error::Unauthorized)?;
        let fingerprint = fingerprint(&("login", &self.generation))?;
        let _op = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return self.receipt(&receipt.op_id),
            Admission::New(op) => op,
        };
        let expires_at = now() + LIFETIME;
        let token = self.token_for(&op)?;
        let mut record = Record::new(op, fingerprint, 201, json!({ "expires_at": expires_at }))?;
        record.session = Some(SessionEffect::Login {
            token_blake3: digest(token.as_str().as_bytes())?,
            expires_at,
            generation: self.generation.clone(),
        });
        self.commit_auth(&record)?;
        self.materialize(record)
    }

    pub fn logout(&self, op: OpId, token: &SessionToken) -> Result<Receipt> {
        let token_blake3 = digest(token.as_str().as_bytes())?;
        let fingerprint = fingerprint(&("logout", &token_blake3))?;
        let _op = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return Ok(receipt),
            Admission::New(op) => op,
        };
        self.authorize(token)?;
        let mut record = Record::new(op, fingerprint, 204, ())?;
        record.session = Some(SessionEffect::Logout { token_blake3 });
        self.commit_auth(&record)?;
        Ok(record.receipt)
    }

    pub fn authorize(&self, token: &SessionToken) -> Result<()> {
        self.healthy()?;
        let hash = digest(token.as_str().as_bytes())?;
        if self.db.session(&hash, &self.generation, now())? {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }

    pub fn receipt(&self, op: &OpId) -> Result<Receipt> {
        self.healthy()?;
        let record = self
            .db
            .record(op)?
            .ok_or(Error::Missing("operation not found"))?;
        self.materialize(record)
    }

    fn commit_auth(&self, record: &Record) -> Result<()> {
        self.serialize_publication(|| {
            self.mark_on_error((|| {
                self.store.start_auth(record)?;
                self.store.store_record(record)?;
                self.index_auth(record)?;
                self.store.end_auth()
            })())
        })
    }

    pub(super) fn index_auth(&self, record: &Record) -> Result<()> {
        self.db.write(|tx| {
            db::put_record(tx, record)?;
            if let Some(effect) = &record.session {
                db::put_session_effect(tx, effect, &self.generation, now())?;
            }
            Ok(())
        })
    }

    fn token_for(&self, op: &OpId) -> Result<SessionToken> {
        let key = blake3::Hash::from_hex(self.config.session_key.as_str()).map_err(config_error)?;
        let token = blake3::keyed_hash(key.as_bytes(), op.as_str().as_bytes());
        SessionToken::parse(token.to_hex().to_string())
    }

    fn materialize(&self, mut record: Record) -> Result<Receipt> {
        if let Some(SessionEffect::Login {
            expires_at,
            generation,
            ..
        }) = record.session
            && bool::from(
                generation
                    .as_str()
                    .as_bytes()
                    .ct_eq(self.generation.as_str().as_bytes()),
            )
        {
            let token = self.token_for(&record.receipt.op_id)?;
            record.receipt.result = json!({ "expires_at": expires_at, "token": token });
        }
        Ok(record.receipt)
    }
}
