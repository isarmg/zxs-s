use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Db, FIND_FOLDER, FIND_PATH, FOLDER_OCCUPIED, LONGEST_PATH, rows};
use crate::domain::{ContainerId, Error, FileName, FolderPath, Result, invalid};

const MOVE_FOLDERS: &str = "UPDATE folders SET
    path=?3||substr(path,length(?2)+1),path_key=?4||substr(path_key,length(?1)+1),
    parent_key=CASE WHEN path_key=?1 THEN ?7 ELSE ?4||substr(parent_key,length(?1)+1) END
    WHERE path_key=?1 OR (path_key>=?5 AND path_key<?6)";
const MOVE_CONTAINERS: &str = "UPDATE containers SET
    folder=?3||substr(folder,length(?2)+1),folder_key=?4||substr(folder_key,length(?1)+1),
    path_key=?4||substr(path_key,length(?1)+1)
    WHERE folder_key=?1 OR (folder_key>=?5 AND folder_key<?6)";

impl Db {
    pub fn require_folder(&self, path: &FolderPath) -> Result<()> {
        require_folder(&*self.read()?, path)
    }

    pub fn prune_candidates(
        &self,
        keep: usize,
        id: Option<&ContainerId>,
    ) -> Result<Vec<(FolderPath, FileName)>> {
        let read = self.read()?;
        let mut query = read.prepare_cached(
            "WITH candidates AS (
                SELECT id,folder,name FROM containers WHERE ?2 IS NULL
                UNION ALL SELECT id,folder,name FROM containers WHERE id=?2
            ) SELECT c.folder,c.name FROM candidates c WHERE EXISTS(
                SELECT 1 FROM versions v WHERE v.id=c.id LIMIT 1 OFFSET ?1)",
        )?;
        let mut rows = query.query(params![
            i64::try_from(keep).unwrap_or(i64::MAX),
            id.map(ContainerId::as_str)
        ])?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next()? {
            paths.push(rows::location(row)?);
        }
        Ok(paths)
    }
}

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
    container_slot(conn, &path, except)
}

pub(super) fn container_slot(
    conn: &Connection,
    path: &FolderPath,
    except: Option<&ContainerId>,
) -> Result<()> {
    let key = path.key();
    if let Some(spelling) = folder_spelling(conn, &key)? {
        return Err(Error::Conflict(format!(
            "the path is occupied by folder {spelling}"
        )));
    }
    if let Some((id, spelling)) = occupied_container(conn, &key)?
        && except.is_none_or(|except| except.as_str() != id)
    {
        return Err(Error::Conflict(format!(
            "the path is occupied by a container: {spelling}"
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
    if let Some((_, spelling)) = occupied_container(conn, &path.key())? {
        return Err(Error::Conflict(format!(
            "the path is occupied by a container: {spelling}"
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
    let (old_len, new_len) = (from.as_str().len(), to.as_str().len());
    if new_len > old_len {
        let longest: usize = conn
            .prepare_cached(LONGEST_PATH)?
            .query_row(params![from_key, start, end], |row| row.get(0))?;
        if longest + (new_len - old_len) > 1024 {
            return Err(Error::Conflict(
                "moving the folder would exceed 1024 bytes in a descendant path".into(),
            ));
        }
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

pub fn put_folder(tx: &Transaction<'_>, folder: &FolderPath) -> Result<()> {
    let changes = tx.prepare_cached(
        "INSERT INTO folders VALUES(?1,?2,?3) ON CONFLICT(path_key) DO UPDATE SET path=excluded.path WHERE path=excluded.path",
    )?.execute(params![folder.key(), folder.parent().as_ref().map_or_else(String::new, FolderPath::key), folder.as_str()])?;
    if changes == 0 {
        return Err(invalid(format!("conflicting directory spelling: {folder}")));
    }
    Ok(())
}

pub(super) fn put_ancestors(tx: &Transaction<'_>, folder: &FolderPath) -> Result<()> {
    let mut parent = Some(folder.clone());
    while let Some(folder) = parent {
        put_folder(tx, &folder)?;
        parent = folder.parent();
    }
    Ok(())
}

pub(super) fn move_folder(tx: &Transaction<'_>, from: &FolderPath, to: &FolderPath) -> Result<()> {
    match require_folder(tx, from) {
        Ok(()) => {}
        Err(Error::Missing(_) | Error::Conflict(_)) => return require_folder(tx, to),
        Err(error) => return Err(error),
    }
    let parent = to.parent().ok_or(Error::Recovery)?;
    put_ancestors(tx, &parent)?;
    let key = from.key();
    let (start, end) = descendants(&key);
    let to_key = to.key();
    let parent_key = parent.key();
    let parameters = params![
        key,
        from.as_str(),
        to.as_str(),
        to_key,
        start,
        end,
        parent_key
    ];
    tx.execute(MOVE_FOLDERS, parameters)?;
    tx.execute(MOVE_CONTAINERS, &parameters[..6])?;
    Ok(())
}

fn ancestors(conn: &Connection, mut current: Option<FolderPath>) -> Result<()> {
    while let Some(folder) = current {
        let key = folder.key();
        if let Some(spelling) = folder_spelling(conn, &key)? {
            if spelling == folder.as_str() {
                return Ok(());
            }
            return Err(Error::Conflict(format!(
                "folder already exists as {spelling}"
            )));
        }
        if let Some((_, spelling)) = occupied_container(conn, &key)? {
            return Err(Error::Conflict(format!(
                "a container occupies ancestor {spelling}"
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

fn occupied_container(conn: &Connection, key: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .prepare_cached(FIND_PATH)?
        .query_row([key], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?)
}
