use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, Params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{FIND_CONTAINER, FIND_VERSIONS};
use crate::domain::{
    Blake3Hash, Container, ContainerId, Detail, Error, FileName, FolderPath, Result, VersionNum,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexedContainer {
    #[serde(rename = "container_id")]
    id: ContainerId,
    tags: Vec<String>,
    latest_version: VersionNum,
    created_at: u64,
    updated_at: u64,
}

pub(super) fn one<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<Option<T>> {
    let json: Option<String> = conn
        .prepare_cached(sql)?
        .query_row(parameters, |r| r.get(0))
        .optional()?;
    json.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}

pub(super) fn many<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<Vec<T>> {
    let mut statement = conn.prepare_cached(sql)?;
    let mut rows = statement.query(parameters)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(serde_json::from_str(&row.get::<_, String>(0)?)?);
    }
    Ok(result)
}

pub(super) fn json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

pub(super) fn container_data(container: &Container) -> Result<String> {
    json(&IndexedContainer {
        id: container.id.clone(),
        tags: container.tags.clone(),
        latest_version: container.latest_version,
        created_at: container.created_at,
        updated_at: container.updated_at,
    })
}

pub(super) fn container(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<Option<Container>> {
    let mut statement = conn.prepare_cached(sql)?;
    let mut rows = statement.query(parameters)?;
    rows.next()?.map(container_row).transpose()
}

pub(super) fn detail(
    conn: &Connection,
    id: &ContainerId,
    check: impl FnOnce(&Container) -> Result<()>,
) -> Result<Option<Detail>> {
    let tx = conn.unchecked_transaction()?;
    let Some(container) = container(&tx, FIND_CONTAINER, [id.as_str()])? else {
        return Ok(None);
    };
    check(&container)?;
    let versions = many(&tx, FIND_VERSIONS, [id.as_str()])?;
    Ok(Some(Detail {
        container,
        versions,
    }))
}

pub(super) fn location(row: &rusqlite::Row<'_>) -> Result<(FolderPath, FileName)> {
    Ok((
        FolderPath::parse(row.get::<_, String>(0)?)?,
        FileName::parse(row.get::<_, String>(1)?)?,
    ))
}

pub(super) fn container_row(row: &rusqlite::Row<'_>) -> Result<Container> {
    let (folder, name) = location(row)?;
    let hash = Blake3Hash::parse(row.get::<_, String>(2)?)?;
    let metadata = row.get::<_, String>(3)?;
    let metadata: IndexedContainer = serde_json::from_str(&metadata)?;
    Ok(Container {
        id: metadata.id,
        folder,
        name,
        tags: metadata.tags,
        latest_version: metadata.latest_version,
        latest_blake3: Some(hash),
        created_at: metadata.created_at,
        updated_at: metadata.updated_at,
    })
}

pub(super) fn open(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

pub(super) fn index_empty(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='containers')",
        [],
        |row| row.get(0),
    )?)
}

pub(super) fn available_reader(
    readers: &[Mutex<Connection>],
    start: usize,
) -> Result<MutexGuard<'_, Connection>> {
    if let Some(connection) = readers
        .iter()
        .cycle()
        .skip(start)
        .take(readers.len())
        .find_map(|reader| reader.try_lock().ok())
    {
        return Ok(connection);
    }
    readers[start].lock().map_err(|_| Error::Recovery)
}
