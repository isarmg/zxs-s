use axum::{body::Body, extract::rejection::JsonRejection, response::IntoResponse};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::{
    Arc, AxState, Blake3Hash, ContainerId, HeaderMap, Json, METADATA_LIMIT, Path, Query, RecycleId,
    Response, Result, Shared, StatusCode, VersionNum, blocking, invalid, token,
};
use crate::service::{UploadHeaders, UploadIntent};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    password: String,
}

pub async fn login(
    AxState(state): AxState<Shared>,
    request: ResultJson<Login>,
) -> Result<Response> {
    let password = Zeroizing::new(checked(request)?.password);
    let session = blocking(move || state.login(&password)).await?;
    Ok(super::session_response(session))
}

pub async fn logout(AxState(state): AxState<Shared>, headers: HeaderMap) -> Result<Response> {
    state.logout(&token(&headers)?)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn create(
    AxState(state): AxState<Shared>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    let intent = UploadIntent::Create(params(&headers)?);
    upload(state, intent, &headers, body).await
}

pub async fn append(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    let intent = UploadIntent::Append(id, params(&headers)?);
    upload(state, intent, &headers, body).await
}

pub async fn patch(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    body: ResultJson<crate::domain::Patch>,
) -> Result<Response> {
    let patch = checked(body)?;
    Ok(Json(blocking(move || state.patch(&id, patch)).await?).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleGuard {
    expected_version: Option<VersionNum>,
}

pub async fn recycle(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
    Query(guard): Query<RecycleGuard>,
) -> Result<Response> {
    Ok(Json(blocking(move || state.recycle(&id, guard.expected_version)).await?).into_response())
}

pub async fn restore(
    AxState(state): AxState<Shared>,
    Path(id): Path<RecycleId>,
    body: ResultJson<crate::domain::Restore>,
) -> Result<Response> {
    let restore = checked(body)?;
    Ok(Json(blocking(move || state.restore(&id, restore)).await?).into_response())
}

pub async fn purge(AxState(state): AxState<Shared>, Path(id): Path<RecycleId>) -> Result<Response> {
    delete(state, Some(id)).await
}

pub async fn purge_all(AxState(state): AxState<Shared>) -> Result<Response> {
    delete(state, None).await
}

async fn delete(state: Shared, id: Option<RecycleId>) -> Result<Response> {
    let purged = blocking(move || state.purge(id.as_ref())).await?;
    Ok(Json(serde_json::json!({"purged": purged})).into_response())
}

async fn upload(
    state: Shared,
    intent: UploadIntent,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response> {
    let hash = headers
        .contains_key("zxs-blake3")
        .then(|| Blake3Hash::parse(required(headers, "zxs-blake3")?))
        .transpose()?;
    let size = required(headers, "content-length")?
        .parse()
        .map_err(|_| invalid("invalid Content-Length"))?;
    if !required(headers, "content-type")?.eq_ignore_ascii_case("application/octet-stream") {
        return Err(invalid("upload requires application/octet-stream"));
    }
    let upload = {
        let state = Arc::clone(&state);
        blocking(move || state.begin_upload(&intent, UploadHeaders { size, hash })).await?
    };
    let _permit = match size {
        0 => None,
        _ => Some(state.transfer(None).await?),
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let (input, output) = tokio::join!(super::stream::feed(body, sender), upload.receive(receiver));
    input?;
    let upload = output?;
    let commit = blocking(move || state.finish_upload(*upload)).await?;
    Ok((StatusCode::CREATED, Json(commit)).into_response())
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
    if encoded.len() > METADATA_LIMIT.div_ceil(3) * 4 {
        return Err(invalid("Zxs-Params metadata exceeds 1 MiB"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("invalid Zxs-Params encoding"))?;
    if bytes.len() > METADATA_LIMIT {
        return Err(invalid("Zxs-Params metadata exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|error| invalid(format!("Zxs-Params: {error}")))
}

pub(super) type ResultJson<T> = std::result::Result<Json<T>, JsonRejection>;

pub(super) fn checked<T>(json: ResultJson<T>) -> Result<T> {
    json.map(|Json(value)| value)
        .map_err(|error| invalid(error.body_text()))
}
