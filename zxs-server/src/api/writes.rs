use axum::{body::Body, extract::rejection::JsonRejection};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::TryStreamExt;
use serde::{Deserialize, de::DeserializeOwned};
use tokio_util::io::StreamReader;
use zeroize::Zeroizing;

use super::{
    Arc, AxState, Blake3Hash, ContainerId, HeaderMap, Json, METADATA_LIMIT, OpId, Patch, Path,
    RecycleId, Response, Restore, Result, Shared, blocking, invalid, reply, token,
};
use crate::service::{BeginUpload, UploadIntent};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    password: String,
}

pub async fn login(
    AxState(state): AxState<Shared>,
    headers: HeaderMap,
    request: ResultJson<Login>,
) -> Result<Response> {
    let op = op(&headers)?;
    let password = Zeroizing::new(checked(request)?.password);
    reply(blocking(move || state.login(op, &password)).await?)
}

pub async fn logout(AxState(state): AxState<Shared>, headers: HeaderMap) -> Result<Response> {
    let op = op(&headers)?;
    let token = token(&headers, None)?;
    reply(blocking(move || state.logout(op, &token)).await?)
}

pub async fn create(
    AxState(state): AxState<Shared>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    let intent = UploadIntent::Create(params(&headers)?);
    upload(state, intent, headers, body).await
}

pub async fn append(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    let intent = UploadIntent::Append(id, params(&headers)?);
    upload(state, intent, headers, body).await
}

pub async fn patch(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    headers: HeaderMap,
    request: ResultJson<Patch>,
) -> Result<Response> {
    let op = op(&headers)?;
    let params = checked(request)?;
    reply(blocking(move || state.patch(op, &id, params)).await?)
}

pub async fn recycle(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    headers: HeaderMap,
) -> Result<Response> {
    let op = op(&headers)?;
    reply(blocking(move || state.recycle(op, &id)).await?)
}

pub async fn restore(
    AxState(state): AxState<Shared>,
    Path(id): Path<RecycleId>,
    headers: HeaderMap,
    request: ResultJson<Restore>,
) -> Result<Response> {
    let op = op(&headers)?;
    let params = checked(request)?;
    reply(blocking(move || state.restore(op, id, params)).await?)
}

async fn upload(
    state: Shared,
    intent: UploadIntent,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    let op = op(&headers)?;
    let hash = Blake3Hash::parse(required(&headers, "zxs-blake3")?)?;
    let size = required(&headers, "content-length")?
        .parse()
        .map_err(|_| invalid("invalid Content-Length"))?;
    if required(&headers, "content-type")? != "application/octet-stream" {
        return Err(invalid("upload requires application/octet-stream"));
    }
    let begin = {
        let state = Arc::clone(&state);
        blocking(move || state.begin_upload(op, intent, size, hash)).await?
    };
    let upload = match begin {
        BeginUpload::Ready(upload) => upload,
        BeginUpload::Replay(receipt) => return reply(receipt),
    };
    let _permit = state.transfer().await?;
    let reader = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
    upload.receive(reader).await?;
    reply(blocking(move || state.finish_upload(*upload)).await?)
}

fn op(headers: &HeaderMap) -> Result<OpId> {
    OpId::parse(required(headers, "zxs-op-id")?)
}

fn required<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().and_then(|v| v.to_str().ok());
    match (value, values.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(invalid(format!("missing or invalid {name} header"))),
    }
}

fn params<T: DeserializeOwned>(headers: &HeaderMap) -> Result<T> {
    let encoded = required(headers, "zxs-params")?;
    if encoded.len() > METADATA_LIMIT {
        return Err(invalid("Zxs-Params is too large"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("invalid Zxs-Params encoding"))?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(format!("Zxs-Params: {error}")))
}

type ResultJson<T> = std::result::Result<Json<T>, JsonRejection>;

fn checked<T>(json: ResultJson<T>) -> Result<T> {
    json.map(|Json(value)| value)
        .map_err(|error| invalid(error.body_text()))
}
