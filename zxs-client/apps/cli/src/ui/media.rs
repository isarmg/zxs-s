use std::io::Read;

use super::{CSP, Reply, Ui, header, progress::Job, read_header};
use tiny_http::{Header, Method, Request, Response, StatusCode};
use zxs_core::{ContainerId, PayloadRequest, Result, VersionNum, invalid};

pub fn serve(state: &Ui, request: &Request, job: &mut Option<Job>) -> Result<Reply> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let (id, version) = path
        .strip_prefix("/media/")
        .and_then(|value| value.split_once('/'))
        .ok_or_else(|| invalid("invalid media path"))?;
    let id = ContainerId::parse(id)?;
    let version: VersionNum = version.parse()?;
    let transfer = query.strip_prefix("download=1&transfer=");
    let download = query == "download=1" || transfer.is_some();
    if request.method() == &Method::Get {
        *job = state.transfers.start(transfer)?;
    }
    let options = PayloadRequest {
        head: request.method() == &Method::Head,
        range: read_header(request, "range"),
    };
    let remote = state
        .remote()?
        .with_progress(job.as_ref().map(Job::observer));
    let payload = remote.payload(&id, version, options)?;
    let headers = response_headers(&payload, download)?;
    let length = payload
        .header("content-length")
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| invalid("invalid upstream payload length"))?;
    let status = StatusCode(payload.status());
    let reader = payload.into_reader();
    let body: Box<dyn Read + Send> = match (job.as_ref(), length) {
        (Some(_), Some(size)) => Box::new(remote.track_download(reader, size as u64)),
        _ => Box::new(reader),
    };
    Ok(Response::new(status, headers, body, length, None))
}

fn response_headers(payload: &zxs_core::Payload, download: bool) -> Result<Vec<Header>> {
    let mut headers = vec![
        header("Cache-Control", "private, no-store")?,
        header("X-Content-Type-Options", "nosniff")?,
        header("Referrer-Policy", "no-referrer")?,
        header("Content-Security-Policy", CSP)?,
    ];
    for name in [
        "Content-Type",
        "Content-Range",
        "Accept-Ranges",
        "ETag",
        "Content-Disposition",
    ] {
        if let Some(value) = payload.header(name) {
            let value = if download && name == "Content-Disposition" {
                value.replacen("inline", "attachment", 1)
            } else {
                value.into()
            };
            headers.push(header(name, &value)?);
        }
    }
    Ok(headers)
}
