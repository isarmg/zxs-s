mod auth;
mod payload;
mod writes;

use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read},
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

pub(crate) use payload::DownloadVersion;
pub use payload::{Payload, PayloadRequest};
pub use writes::UploadFile;

#[derive(Clone)]
pub struct Remote {
    agent: Agent,
    endpoint: String,
    auth: Arc<Mutex<auth::Credentials>>,
}

impl Remote {
    /// # Errors
    ///
    /// Returns an error if the HTTP origin is invalid.
    pub fn connect(endpoint: &str, token: Option<SessionToken>) -> Result<Self> {
        let endpoint = origin(endpoint)?;
        let config = Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_resolve(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_await_100(Some(Duration::from_secs(5)))
            .timeout_per_read(Some(Duration::from_secs(10)))
            .timeout_per_write(Some(Duration::from_secs(10)))
            .build();
        Ok(Self {
            agent: Agent::new_with_config(config),
            endpoint,
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

    pub(crate) fn descendant_folders(&self, parent: &FolderPath) -> Result<FolderPage> {
        self.get(&format!("{}&recursive=true", folder_url(parent)))
    }

    /// Reads the current metadata for known IDs in batches. Missing IDs are omitted.
    ///
    /// # Errors
    /// Returns an error if a batch cannot be read from the server.
    pub fn lookup_containers<'a>(
        &self,
        ids: impl Iterator<Item = &'a ContainerId>,
    ) -> Result<BTreeMap<ContainerId, Container>> {
        const MAX_IDS: usize = 100;
        let ids: Vec<_> = ids.map(ContainerId::as_str).collect();
        let mut containers = BTreeMap::new();
        for chunk in ids.chunks(MAX_IDS) {
            let query = query(&[("ids", Some(&chunk.join(",")))]);
            let page: ContainerPage = self.get(&format!("/containers{query}"))?;
            containers.extend(
                page.containers
                    .into_iter()
                    .map(|container| (container.container_id.clone(), container)),
            );
        }
        Ok(containers)
    }

    pub(crate) fn summary(&self, id: &ContainerId) -> Result<Container> {
        self.lookup_containers(std::iter::once(id))?
            .remove(id)
            .ok_or_else(|| Error::Remote {
                status: 404,
                message: "container not found".into(),
            })
    }

    /// # Errors
    ///
    /// Returns an error if the container cannot be found or the server response is invalid.
    pub fn detail(&self, id: &ContainerId) -> Result<Detail> {
        let detail: Detail = self.get(&format!("/containers/{id}"))?;
        if detail.container.container_id != *id {
            return Err(invalid("服务器返回了不同的文件容器"));
        }
        Ok(detail)
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
            .uri(format!("{}{path}", self.endpoint))
    }

    fn send(&self, request: Builder, body: impl ureq::AsSendBody) -> Result<Response<Body>> {
        let request = request
            .body(body)
            .map_err(|error| invalid(error.to_string()))?;
        self.agent.run(request).map_err(Into::into)
    }
}

fn origin(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    let uri: Uri = endpoint
        .parse()
        .map_err(|_| invalid("enter an HTTP server address such as http://127.0.0.1:7738"))?;
    if uri.scheme_str() != Some("http") || uri.path() != "/" || uri.query().is_some() {
        return Err(invalid("the server address must be an HTTP origin"));
    }
    let authority = uri
        .authority()
        .filter(|a| !a.as_str().contains('@') && !a.host().is_empty())
        .ok_or_else(|| invalid("the server address requires a host"))?;
    if authority.host() != authority.as_str() && authority.port_u16().is_none_or(|port| port == 0) {
        return Err(invalid("server port must be between 1 and 65535"));
    }
    Ok(format!("http://{authority}"))
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
