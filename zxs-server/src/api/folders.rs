use axum::response::IntoResponse;
use serde::Deserialize;

use crate::domain::{Change, Folder, FolderMove, FolderPage, FolderPath};

use super::{
    AxState, Json, Query, Response, Result, Shared, StatusCode, blocking,
    writes::{ResultJson, checked},
};

pub async fn list(
    AxState(state): AxState<Shared>,
    Query(input): Query<Folder>,
) -> Result<Json<FolderPage>> {
    blocking(move || state.folders(&input.folder))
        .await
        .map(Json)
}

pub async fn create(
    AxState(state): AxState<Shared>,
    request: ResultJson<Folder>,
) -> Result<Response> {
    let change = Change::CreateFolder {
        folder: checked(request)?.folder,
    };
    let (folder, created) = blocking(move || state.edit_folder(change)).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(folder)).into_response())
}

pub async fn relocate(
    AxState(state): AxState<Shared>,
    request: ResultJson<FolderMove>,
) -> Result<Json<Folder>> {
    let FolderMove { from, to } = checked(request)?;
    let (folder, _) = blocking(move || {
        state.edit_folder(Change::MoveFolder {
            from,
            to,
            staged: false,
        })
    })
    .await?;
    Ok(Json(folder))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveFolder {
    folder: FolderPath,
    #[serde(default)]
    recursive: bool,
}

pub async fn remove(
    AxState(state): AxState<Shared>,
    Query(input): Query<RemoveFolder>,
) -> Result<Json<Folder>> {
    blocking(move || state.remove_folder(input.folder, input.recursive))
        .await
        .map(Json)
}
