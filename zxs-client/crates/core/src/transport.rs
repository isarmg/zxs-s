mod auth;
mod payload;
mod writes;

use std::{
    io::{self, BufReader, Read},
    net::ToSocketAddrs,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::de::DeserializeOwned;
use ureq::{
    Agent, Body,
    http::{Method, Request, Response, Uri, request::Builder},
};

use crate::domain::{
    BUFFER, Blake3Hash, Commit, Container, ContainerId, ContainerPage, Detail, Error, Filter,
    Folder, FolderMove, FolderPage, FolderPath, METADATA_LIMIT, NewContainer, NewVersion, Patch,
    RecycleFilter, RecycleId, RecyclePage, Recycled, Restore, Result, Session, SessionToken,
    Version, VersionNum, invalid,
};

pub use payload::{Payload, PayloadRequest};
pub use writes::UploadFile;

#[derive(Clone)]
pub struct Remote {
    agent: Agent,
    endpoint: String,
    address: String,
    auth: Arc<Mutex<auth::Credentials>>,
}

impl Remote {
    /// # Errors
    ///
    /// Returns an error if the HTTP origin cannot be resolved to a server address.
    pub fn connect(endpoint: &str, token: Option<SessionToken>) -> Result<Self> {
        let (endpoint, address) = resolve(endpoint)?;
        let config = Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_await_100(Some(Duration::from_secs(5)))
            .build();
        Ok(Self {
            agent: Agent::new_with_config(config),
            endpoint,
            address,
            auth: Arc::new(Mutex::new(auth::Credentials::new(token))),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    /// # Errors
    ///
    /// Returns an error if the server rejects the query or the response cannot be read.
    pub fn list(&self, filter: &Filter) -> Result<ContainerPage> {
        let limit = filter.limit.map(|n| n.to_string());
        let query = query(&[
            ("folder", filter.folder.as_ref().map(FolderPath::as_str)),
            ("tag", filter.tag.as_deref()),
            ("q", filter.q.as_deref()),
            ("cursor", filter.cursor.as_deref()),
            ("limit", limit.as_deref()),
        ]);
        self.get(&format!("/containers{query}"))
    }

    /// # Errors
    /// Returns an error if the parent does not exist or its direct children cannot be read.
    pub fn folders(&self, parent: &FolderPath) -> Result<FolderPage> {
        self.get(&folder_url(parent))
    }

    /// # Errors
    ///
    /// Returns an error if the container cannot be found or the server response is invalid.
    pub fn detail(&self, id: &ContainerId) -> Result<Detail> {
        self.get(&format!("/containers/{id}"))
    }

    /// # Errors
    ///
    /// Returns an error if the recycle list cannot be queried or decoded.
    pub fn recycle(&self, filter: &RecycleFilter) -> Result<RecyclePage> {
        let limit = filter.limit.map(|n| n.to_string());
        let query = query(&[
            ("cursor", filter.cursor.as_deref()),
            ("limit", limit.as_deref()),
        ]);
        self.get(&format!("/recycle{query}"))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.authorized(Method::GET, path, |request| self.send(request, ()))?;
        decode(checked(response)?)
    }

    fn request(&self, method: Method, path: &str) -> Builder {
        Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.address))
    }

    fn send(&self, request: Builder, body: impl ureq::AsSendBody) -> Result<Response<Body>> {
        let request = request
            .body(body)
            .map_err(|error| invalid(error.to_string()))?;
        self.agent.run(request).map_err(Into::into)
    }
}

fn resolve(endpoint: &str) -> Result<(String, String)> {
    let endpoint = endpoint.trim();
    let uri: Uri = endpoint
        .parse()
        .map_err(|_| invalid("enter an HTTP server address such as http://127.0.0.1:7738"))?;
    if uri.scheme_str() != Some("http") || uri.path() != "/" || uri.query().is_some() {
        return Err(invalid("the server address must be an HTTP origin"));
    }
    let authority = uri
        .authority()
        .filter(|a| !a.as_str().contains('@'))
        .ok_or_else(|| invalid("the server address requires a host"))?;
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = authority.port_u16().unwrap_or(80);
    if port == 0 {
        return Err(invalid("server port cannot be zero"));
    }
    let address = (host, port)
        .to_socket_addrs()?
        .filter(|address| !address.ip().is_unspecified())
        .min_by_key(|address| u8::from(host.eq_ignore_ascii_case("localhost") && address.is_ipv6()))
        .ok_or_else(|| invalid("no server address was found"))?;
    Ok((format!("http://{authority}"), format!("http://{address}")))
}

fn checked(response: Response<Body>) -> Result<Response<Body>> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(remote_error(response))
    }
}

fn remote_error(response: Response<Body>) -> Error {
    let status = response.status().as_u16();
    let fallback = response
        .status()
        .canonical_reason()
        .unwrap_or("request failed")
        .to_owned();
    let message = limited(response.into_body().into_reader(), METADATA_LIMIT as u64)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|json| {
            json.get("message")
                .and_then(|s| s.as_str())
                .map(str::to_owned)
        })
        .unwrap_or(fallback);
    Error::Remote { status, message }
}

fn limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("metadata response exceeds its size limit"));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(response: Response<Body>) -> Result<T> {
    Ok(serde_json::from_reader(BufReader::new(
        response.into_body().into_reader(),
    ))?)
}

fn query(fields: &[(&str, Option<&str>)]) -> String {
    use std::fmt::Write;
    let mut output = String::new();
    for (key, value) in fields.iter().filter_map(|(k, v)| v.map(|v| (k, v))) {
        output.push(if output.is_empty() { '?' } else { '&' });
        output.push_str(key);
        output.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                output.push(char::from(byte));
            } else {
                let _ = write!(output, "%{byte:02X}");
            }
        }
    }
    output
}

fn folder_url(parent: &FolderPath) -> String {
    format!("/folders{}", query(&[("folder", Some(parent.as_str()))]))
}

fn read_block(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}
