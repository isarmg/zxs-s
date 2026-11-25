use rusqlite::{Connection, OptionalExtension, params};

use super::{FIND_FOLDER, FIND_PATH, FOLDER_OCCUPIED, LONGEST_PATH};
use crate::domain::{ContainerId, Error, FileName, FolderPath, Result, invalid};

pub(super) fn descendants(key: &str) -> (String, String) {
    let trimmed = key.trim_end_matches('/');
    (format!("{trimmed}/"), format!("{trimmed}0"))
}

pub(super) fn container_target(
    conn: &Connection,
    folder: &FolderPath,
    name: &FileName,
    except: Option<&ContainerId>,
) -> Result<()> {
    let path = folder.child(name.as_str())?;
    ancestors(conn, Some(folder.clone()))?;
    if let Some(spelling) = folder_spelling(conn, &path.key())? {
        return Err(Error::Conflict(format!(
            "the path is occupied by folder {spelling}"
        )));
    }
    if let Some(id) = container_id(conn, &path.key())?
        && except.is_none_or(|except| except.as_str() != id)
    {
        return Err(Error::Conflict(format!(
            "the path is occupied by a container: {path}"
        )));
    }
    Ok(())
}

pub(super) fn folder_target(
    conn: &Connection,
    path: &FolderPath,
    except: Option<&FolderPath>,
) -> Result<()> {
    ancestors(conn, path.parent())?;
    if let Some(spelling) = folder_spelling(conn, &path.key())?
        && except.is_none_or(|except| except.as_str() != spelling)
    {
        return Err(Error::Conflict(format!(
            "folder already exists as {spelling}"
        )));
    }
    if container_id(conn, &path.key())?.is_some() {
        return Err(Error::Conflict(format!(
            "the path is occupied by a container: {path}"
        )));
    }
    Ok(())
}

pub(super) fn require_folder(conn: &Connection, path: &FolderPath) -> Result<()> {
    match folder_spelling(conn, &path.key())? {
        Some(existing) if existing == path.as_str() => Ok(()),
        Some(existing) => Err(Error::Conflict(format!(
            "folder already exists as {existing}"
        ))),
        None => Err(Error::Missing("folder not found")),
    }
}

pub(super) fn check_move(conn: &Connection, from: &FolderPath, to: &FolderPath) -> Result<()> {
    let from_key = from.key();
    let to_key = to.key();
    if from.as_str() == "/" || to.as_str() == "/" {
        return Err(invalid("the root folder cannot be moved"));
    }
    let (start, end) = descendants(&from_key);
    if to_key.starts_with(&start) {
        return Err(Error::Conflict(
            "a folder cannot move into its own subtree".into(),
        ));
    }
    require_folder(conn, from)?;
    folder_target(conn, to, Some(from))?;
    let longest: usize = conn
        .prepare_cached(LONGEST_PATH)?
        .query_row(params![from_key, start, end], |row| row.get(0))?;
    if longest - from.as_str().len() + to.as_str().len() > 1024 {
        return Err(Error::Conflict(
            "moving the folder would exceed 1024 bytes in a descendant path".into(),
        ));
    }
    Ok(())
}

pub(super) fn check_delete(conn: &Connection, folder: &FolderPath) -> Result<()> {
    if folder.as_str() == "/" {
        return Err(invalid("the root folder cannot be deleted"));
    }
    require_folder(conn, folder)?;
    let key = folder.key();
    let (start, end) = descendants(&key);
    let occupied = conn
        .prepare_cached(FOLDER_OCCUPIED)?
        .query_row(params![key, start, end], |row| row.get::<_, bool>(0))?;
    if occupied {
        return Err(Error::Conflict("the folder is not empty".into()));
    }
    Ok(())
}

fn ancestors(conn: &Connection, mut current: Option<FolderPath>) -> Result<()> {
    while let Some(folder) = current {
        let key = folder.key();
        if let Some(spelling) = folder_spelling(conn, &key)?
            && spelling != folder.as_str()
        {
            return Err(Error::Conflict(format!(
                "folder already exists as {spelling}"
            )));
        }
        if container_id(conn, &key)?.is_some() {
            return Err(Error::Conflict(format!(
                "a container occupies ancestor {folder}"
            )));
        }
        current = folder.parent();
    }
    Ok(())
}

fn folder_spelling(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(FIND_FOLDER)?
        .query_row([key], |row| row.get(0))
        .optional()?)
}

fn container_id(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(FIND_PATH)?
        .query_row([key], |row| row.get(0))
        .optional()?)
}
