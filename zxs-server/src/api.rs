mod payload;
mod writes;

use std::{future::Future, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State as AxState},
    http::{HeaderMap, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::json;

use crate::{
    domain::{
        BUFFER, Blake3Hash, ContainerId, ContainerPage, Detail, Error, FileName, Filter,
        METADATA_LIMIT, OpId, Patch, Receipt, RecycleFilter, RecycleId, RecyclePage, Restore,
        Result, SessionToken, VersionNum, invalid, media_type,
    },
    service::State,
};

type Shared = Arc<State>;

pub fn router(state: Shared) -> Router {
    let protected = Router::new()
        .route("/containers", get(list).post(writes::create))
        .route(
            "/containers/{id}",
            get(detail).patch(writes::patch).delete(writes::recycle),
        )
        .route("/containers/{id}/versions", post(writes::append))
        .route(
            "/containers/{id}/versions/{version}/payload",
            get(payload::get).head(payload::head),
        )
        .route("/ops/{op}", get(operation))
        .route("/recycle", get(recycle))
        .route("/recycle/{id}/restore", post(writes::restore))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authenticate,
        ));
    Router::new()
        .route("/sessions", post(writes::login))
        .route("/sessions/current", delete(writes::logout))
        .merge(protected)
        .layer(DefaultBodyLimit::max(METADATA_LIMIT))
        .with_state(state)
}

async fn authenticate(
    AxState(state): AxState<Shared>,
    request: Request,
    next: Next,
) -> Result<Response> {
    let token = token(request.headers(), Some(request.uri()))?;
    blocking(move || state.authorize(&token)).await?;
    Ok(next.run(request).await)
}

fn token(headers: &HeaderMap, uri: Option<&Uri>) -> Result<SessionToken> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value.to_str().map_err(|_| Error::Unauthorized)?;
        let (scheme, token) = value.split_once(' ').ok_or(Error::Unauthorized)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(Error::Unauthorized);
        }
        return SessionToken::parse(token).map_err(|_| Error::Unauthorized);
    }
    let query = uri
        .filter(|u| u.path().ends_with("/payload"))
        .and_then(Uri::query)
        .unwrap_or("");
    let mut tokens = query.split('&').filter_map(|p| p.strip_prefix("token="));
    match (tokens.next(), tokens.next()) {
        (Some(token), None) => SessionToken::parse(token).map_err(|_| Error::Unauthorized),
        _ => Err(Error::Unauthorized),
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(job).await?
}

async fn list(
    AxState(state): AxState<Shared>,
    Query(filter): Query<Filter>,
) -> Result<Json<ContainerPage>> {
    blocking(move || state.list(filter)).await.map(Json)
}

async fn detail(
    AxState(state): AxState<Shared>,
    Path(id): Path<ContainerId>,
) -> Result<Json<Detail>> {
    blocking(move || state.detail(&id)).await.map(Json)
}

async fn recycle(
    AxState(state): AxState<Shared>,
    Query(filter): Query<RecycleFilter>,
) -> Result<Json<RecyclePage>> {
    blocking(move || state.recycle_list(&filter))
        .await
        .map(Json)
}

async fn operation(AxState(state): AxState<Shared>, Path(op): Path<OpId>) -> Result<Json<Receipt>> {
    blocking(move || state.receipt(&op)).await.map(Json)
}

fn reply(receipt: Receipt) -> Result<Response> {
    let status = StatusCode::from_u16(receipt.status).map_err(|_| Error::Recovery)?;
    let mut response = if status == StatusCode::NO_CONTENT {
        status.into_response()
    } else {
        (status, Json(receipt.result)).into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request", self.to_string()),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.to_string()),
            Self::Missing(_) => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict", self.to_string()),
            Self::Reused => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "op_id_reused",
                self.to_string(),
            ),
            Self::Recovery => (
                StatusCode::SERVICE_UNAVAILABLE,
                "recovery_required",
                self.to_string(),
            ),
            _ => {
                tracing::error!(error = ?self, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "storage operation failed".into(),
                )
            }
        };
        let mut response =
            (status, Json(json!({ "code": code, "message": message }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

pub async fn serve(
    state: State,
    listen: std::net::SocketAddr,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(listen = %listener.local_addr()?, "ZXS listening");
    axum::serve(listener, router(Arc::new(state)))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}
