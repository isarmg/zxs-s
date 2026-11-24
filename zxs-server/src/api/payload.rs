use axum::{body::Body, http::HeaderValue};
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::{
    Arc, AxState, BUFFER, ContainerId, Error, FileName, HeaderMap, Path, Response, Result, Shared,
    StatusCode, VersionNum, blocking, header, media_type,
};

pub async fn get(
    AxState(state): AxState<Shared>,
    Path(path): Path<(ContainerId, VersionNum)>,
    headers: HeaderMap,
) -> Result<Response> {
    send(state, path, headers, false).await
}

pub async fn head(
    AxState(state): AxState<Shared>,
    Path(path): Path<(ContainerId, VersionNum)>,
    headers: HeaderMap,
) -> Result<Response> {
    send(state, path, headers, true).await
}

async fn send(
    state: Shared,
    (id, number): (ContainerId, VersionNum),
    headers: HeaderMap,
    head: bool,
) -> Result<Response> {
    let (version, file) = {
        let state = Arc::clone(&state);
        blocking(move || state.open_payload(&id, number)).await?
    };
    let etag = format!("\"{}\"", version.blake3);
    let mut response = Response::new(Body::empty());
    set(&mut response, header::ETAG, &etag)?;
    set(&mut response, header::ACCEPT_RANGES, "bytes")?;
    set(&mut response, header::CACHE_CONTROL, "private, no-store")?;
    set(&mut response, header::REFERRER_POLICY, "no-referrer")?;
    set(&mut response, header::X_CONTENT_TYPE_OPTIONS, "nosniff")?;
    let Ok(span) = range(&headers, version.size) else {
        *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
        set(
            &mut response,
            header::CONTENT_RANGE,
            &format!("bytes */{}", version.size),
        )?;
        return Ok(response);
    };
    let (start, length) = if let Some((start, end)) = span {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        set(
            &mut response,
            header::CONTENT_RANGE,
            &format!("bytes {start}-{end}/{}", version.size),
        )?;
        (start, end - start + 1)
    } else {
        (0, version.size)
    };
    set(
        &mut response,
        header::CONTENT_TYPE,
        media_type(&version.name_at_commit),
    )?;
    set(&mut response, header::CONTENT_LENGTH, &length.to_string())?;
    set(
        &mut response,
        header::CONTENT_DISPOSITION,
        &disposition(&version.name_at_commit),
    )?;
    if !head && length > 0 {
        let mut file = tokio::fs::File::from_std(file);
        if start > 0 {
            file.seek(std::io::SeekFrom::Start(start)).await?;
        }
        let permit = state.transfer().await?;
        let stream = ReaderStream::with_capacity(file.take(length), BUFFER).map(move |chunk| {
            let _permit = &permit;
            chunk
        });
        *response.body_mut() = Body::from_stream(stream);
    }
    Ok(response)
}

fn set(response: &mut Response, name: header::HeaderName, value: &str) -> Result<()> {
    let value = HeaderValue::from_str(value).map_err(|_| Error::Recovery)?;
    response.headers_mut().insert(name, value);
    Ok(())
}

fn range(headers: &HeaderMap, size: u64) -> std::result::Result<Option<(u64, u64)>, ()> {
    let Some(range) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    if headers.get_all(header::RANGE).iter().count() != 1 {
        return Err(());
    }
    let (unit, value) = range.to_str().map_err(|_| ())?.split_once('=').ok_or(())?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Ok(None);
    }
    if size == 0 || value.contains(',') {
        return Err(());
    }
    let (left, right) = value.split_once('-').ok_or(())?;
    if left.is_empty() {
        let suffix = number(right)?;
        return if suffix == 0 {
            Err(())
        } else {
            Ok(Some((size.saturating_sub(suffix), size - 1)))
        };
    }
    let start = number(left)?;
    let end = if right.is_empty() {
        size - 1
    } else {
        number(right)?.min(size - 1)
    };
    if start > end {
        Err(())
    } else {
        Ok(Some((start, end)))
    }
}

fn number(text: &str) -> std::result::Result<u64, ()> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

fn disposition(name: &FileName) -> String {
    use std::fmt::Write;
    let mut output = "inline; filename*=UTF-8''".to_owned();
    for byte in name.as_str().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}
