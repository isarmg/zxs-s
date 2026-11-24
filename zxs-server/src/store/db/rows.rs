use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Params};
use serde::{Serialize, de::DeserializeOwned};

use crate::domain::{Error, FolderPath, FolderSummary, Result};

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

pub(super) fn summarize(
    folder: &FolderPath,
    prefix: &str,
    mut rows: rusqlite::Rows<'_>,
) -> Result<Vec<FolderSummary>> {
    let mut counts = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let tail = path.strip_prefix(prefix).ok_or(Error::Recovery)?;
        let name = tail.split('/').next().ok_or(Error::Recovery)?;
        *counts.entry(folder.child(name)?).or_insert(0) += row.get::<_, u64>(1)?;
    }
    Ok(counts
        .into_iter()
        .map(|(folder, count)| FolderSummary { folder, count })
        .collect())
}
