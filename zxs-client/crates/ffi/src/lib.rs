use std::{
    ffi::{CString, c_char},
    path::{Path, PathBuf},
    ptr, slice,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    ContainerId, Error, FileName, Filter, FolderPath, HashedFile, METADATA_LIMIT, NewContainer,
    Remote, Result, SessionToken, VersionNum, invalid, media_type, new_op, portable_component,
    random_hex,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    server: String,
    token: SessionToken,
}

impl Connection {
    fn open(&self) -> Result<Remote> {
        Remote::connect(&self.server, false, Some(self.token.clone()))
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Login {
        server: String,
        password: String,
    },
    Logout {
        connection: Connection,
    },
    Browse {
        connection: Connection,
        folder: FolderPath,
        cursor: Option<ContainerId>,
    },
    Backup {
        connection: Connection,
        file: PathBuf,
        name: FileName,
        folder: FolderPath,
    },
    Media {
        connection: Connection,
        id: ContainerId,
        version: Option<VersionNum>,
    },
    Document {
        connection: Connection,
        id: ContainerId,
        version: Option<VersionNum>,
        cache: PathBuf,
    },
}

/// Dispatches a UTF-8 request and returns a NUL-terminated UTF-8 response.
///
/// # Safety
///
/// request must be readable for length bytes for the duration of this call.
/// Release the returned pointer exactly once with `zxs_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zxs_call(request: *const u8, length: usize) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        if request.is_null() || length == 0 || length > METADATA_LIMIT {
            return Err(invalid("invalid C ABI request"));
        }
        // The foreign caller provides the buffer and keeps it alive for this call.
        let bytes = unsafe { slice::from_raw_parts(request, length) };
        run(serde_json::from_slice(bytes)?)
    });
    let value = match result {
        Ok(Ok(data)) => json!({ "ok": true, "data": data }),
        Ok(Err(error)) => {
            json!({ "ok": false, "status": error.status(), "error": error.to_string() })
        }
        Err(_) => json!({ "ok": false, "status": 500, "error": "unexpected C ABI error" }),
    };
    serde_json::to_vec(&value)
        .ok()
        .and_then(|bytes| CString::new(bytes).ok())
        .map_or(ptr::null_mut(), CString::into_raw)
}

/// Releases a response allocated by `zxs_call`.
///
/// # Safety
///
/// value must be null or an unfreed response pointer from `zxs_call`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zxs_free(value: *mut c_char) {
    if !value.is_null() {
        // Only zxs_call allocates a response for this ownership transfer.
        drop(unsafe { CString::from_raw(value) });
    }
}

fn run(request: Request) -> Result<Value> {
    match request {
        Request::Login { server, password } => {
            let remote = Remote::connect(&server, false, None)?;
            let session = remote.login(&password)?;
            Ok(json!({ "server": remote.endpoint(), "session": session }))
        }
        Request::Logout { connection } => {
            match connection.open()?.logout() {
                Ok(()) | Err(Error::Remote { status: 401, .. }) => {}
                Err(error) => return Err(error),
            }
            Ok(json!({}))
        }
        Request::Browse {
            connection,
            folder,
            cursor,
        } => Ok(serde_json::to_value(connection.open()?.list(&Filter {
            folder: Some(folder),
            cursor,
            ..Default::default()
        })?)?),
        Request::Backup {
            connection,
            file,
            name,
            folder,
        } => backup(&connection.open()?, &file, name, folder),
        Request::Media {
            connection,
            id,
            version,
        } => {
            let remote = connection.open()?;
            let (_, version) = remote.version(&id, version)?;
            Ok(
                json!({ "url": remote.media_url(&id, version.version)?, "type": media_type(&version.name_at_commit) }),
            )
        }
        Request::Document {
            connection,
            id,
            version,
            cache,
        } => document(&connection.open()?, &id, version, &cache),
    }
}

fn backup(remote: &Remote, file: &Path, name: FileName, folder: FolderPath) -> Result<Value> {
    if !file.is_file() {
        return Err(invalid("backup requires a local file"));
    }
    let hashed = HashedFile::open(file)?;
    if let Some(id) = locate(remote, &folder, &name)? {
        let (container, version) = remote.version(&id, None)?;
        if version.size == hashed.size && version.blake3 == hashed.hash {
            return Ok(
                json!({ "already_present": true, "container": container, "version": version }),
            );
        }
        let commit = remote.append(new_op()?, &id, version.version, hashed)?;
        return Ok(serde_json::to_value(commit)?);
    }
    let commit = remote.create(
        new_op()?,
        &NewContainer {
            folder,
            name,
            tags: Vec::new(),
        },
        hashed,
    )?;
    Ok(serde_json::to_value(commit)?)
}

fn locate(remote: &Remote, folder: &FolderPath, name: &FileName) -> Result<Option<ContainerId>> {
    let mut filter = Filter {
        folder: Some(folder.clone()),
        q: Some(name.to_string()),
        ..Default::default()
    };
    loop {
        let page = remote.list(&filter)?;
        if let Some(entry) = page
            .containers
            .into_iter()
            .find(|entry| entry.name.key() == name.key())
        {
            return Ok(Some(entry.container_id));
        }
        filter.cursor = page.next_cursor;
        if filter.cursor.is_none() {
            return Ok(None);
        }
    }
}

fn document(
    remote: &Remote,
    id: &ContainerId,
    number: Option<VersionNum>,
    cache: &Path,
) -> Result<Value> {
    let (container, version) = remote.version(id, number)?;
    let name = portable_component(container.name.as_str())
        .unwrap_or_else(|_| version.payload_file.clone());
    let directory = cache.join(random_hex::<16>()?);
    std::fs::create_dir_all(cache)?;
    std::fs::create_dir(&directory)?;
    let file = directory.join(name.as_str());
    if let Err(error) = remote.download_new(id, &version, &file) {
        let _ = std::fs::remove_dir(&directory);
        return Err(error);
    }
    Ok(json!({ "path": file, "type": media_type(&version.name_at_commit) }))
}
