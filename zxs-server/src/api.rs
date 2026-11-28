mod folders;
mod payload;
mod stream;
mod writes;

use std::{future::Future, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State as AxState},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::json;

use crate::{
    domain::{
        BUFFER, Blake3Hash, ContainerId, ContainerPage, Error, FileName, Filter, METADATA_LIMIT,
        RecycleFilter, RecycleId, RecyclePage, Result, SessionToken, VersionNum, invalid,
        media_type,
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
        .route("/recycle", get(recycle).delete(writes::purge_all))
        .route("/recycle/{id}", delete(writes::purge))
        .route("/recycle/{id}/restore", post(writes::restore))
        .route(
            "/folders",
            get(folders::list)
                .post(folders::create)
                .patch(folders::relocate)
                .delete(folders::remove),
        )
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
    state.authorize(&token(request.headers())?)?;
    Ok(next.run(request).await)
}

fn token(headers: &HeaderMap) -> Result<SessionToken> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(Error::Unauthorized)?;
    let value = value.to_str().map_err(|_| Error::Unauthorized)?;
    let (scheme, token) = value.split_once(' ').ok_or(Error::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(Error::Unauthorized);
    }
    SessionToken::parse(token).map_err(|_| Error::Unauthorized)
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
) -> Result<Json<crate::domain::Detail>> {
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

fn session_response(session: crate::domain::Session) -> Response {
    let mut response = (StatusCode::CREATED, Json(session)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request", self.to_string()),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.to_string()),
            Self::Missing(_) => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict", self.to_string()),
            Self::FolderMoving(_) => (StatusCode::LOCKED, "folder_moving", self.to_string()),
            Self::Recovery => (
                StatusCode::SERVICE_UNAVAILABLE,
                "recovery_required",
                self.to_string(),
            ),
            Self::RecycleFull => (
                StatusCode::INSUFFICIENT_STORAGE,
                "recycle_storage_full",
                self.to_string(),
            ),
            Self::RecycleOffline => (
                StatusCode::INSUFFICIENT_STORAGE,
                "recycle_storage_offline",
                self.to_string(),
            ),
            Self::Io(ref error) if error.kind() == std::io::ErrorKind::StorageFull => (
                StatusCode::INSUFFICIENT_STORAGE,
                "storage_full",
                "storage is full".into(),
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
    state: Shared,
    listen: std::net::SocketAddr,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(listen = %listener.local_addr()?, "ZXS listening");
    tokio::select! {
        result = axum::serve(listener, router(state)) => result?,
        () = shutdown => {},
    }
    Ok(())
}
