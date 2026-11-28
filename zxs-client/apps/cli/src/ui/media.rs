use std::io::Read;

use super::{Reply, Ui, header, read_header};
use tiny_http::{Header, Method, Request, Response, StatusCode};
use zxs_core::{ContainerId, PayloadRequest, Result, VersionNum, invalid};

pub fn serve(state: &Ui, request: &Request) -> Result<Reply> {
    let url = request.url();
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let (id, version) = path
        .strip_prefix("/media/")
        .and_then(|value| value.split_once('/'))
        .ok_or_else(|| invalid("invalid media path"))?;
    let id = ContainerId::parse(id)?;
    let version: VersionNum = version.parse()?;
    let options = PayloadRequest {
        head: request.method() == &Method::Head,
        range: read_header(request, "range"),
    };
    let root = state
        .root
        .read()
        .map_err(|error| invalid(error.to_string()))?;
    let profile = state.targeted_profile(&root, query.strip_prefix("target="))?;
    let payload = state.remote(profile)?.payload(&id, version, options)?;
    let headers = response_headers(&payload)?;
    let status = StatusCode(payload.status());
    let length = payload
        .header("content-length")
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| invalid("invalid upstream payload length"))?;
    let body: Box<dyn Read + Send> = Box::new(payload.into_reader());
    Ok(Response::new(status, headers, body, length, None))
}

fn response_headers(payload: &zxs_core::Payload) -> Result<Vec<Header>> {
    let mut headers = Vec::new();
    for name in [
        "Content-Type",
        "Content-Range",
        "Accept-Ranges",
        "ETag",
        "Content-Disposition",
    ] {
        if let Some(value) = payload.header(name) {
            headers.push(header(name, value)?);
        }
    }
    Ok(headers)
}
