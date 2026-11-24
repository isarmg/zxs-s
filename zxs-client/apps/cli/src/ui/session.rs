use serde::Deserialize;
use serde_json::json;

use super::{Reply, header, json_input, json_response, read_header};
use std::sync::Mutex;
use tiny_http::Request;
use zxs_core::{Error, Result, invalid, random_hex};

pub struct Access {
    name: String,
    entrance: Mutex<Option<String>>,
    session: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    token: String,
}

impl Access {
    pub fn new(port: u16) -> Result<(Self, String)> {
        let token = random_hex::<32>()?;
        Ok((
            Self {
                name: format!("zxs_{port}"),
                entrance: Mutex::new(Some(token.clone())),
                session: random_hex::<32>()?,
            },
            token,
        ))
    }

    pub fn authorize(&self, request: &mut Request) -> Result<Reply> {
        let key: Key = json_input(request)?;
        let mut entrance = self
            .entrance
            .lock()
            .map_err(|error| invalid(error.to_string()))?;
        if entrance.as_deref() != Some(&key.token) {
            return Err(Error::Remote {
                status: 401,
                message: "access URL is invalid or already used".into(),
            });
        }
        *entrance = None;
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict",
            self.name, self.session
        );
        Ok(json_response(&json!({ "ok": true })).with_header(header("Set-Cookie", &cookie)?))
    }

    pub fn valid(&self, request: &Request) -> bool {
        read_header(request, "cookie")
            .into_iter()
            .flat_map(|header| header.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .any(|(name, value)| name == self.name && value == self.session)
    }
}
