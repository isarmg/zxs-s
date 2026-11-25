mod backup;
mod cache;

use std::{
    ffi::{CString, c_char},
    path::PathBuf,
    ptr, slice,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, ContainerId, Error, FileName, Filter, FolderPath, METADATA_LIMIT, Remote, Result,
    VersionNum, invalid, media_type,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    server: String,
    password: String,
}

static REMOTE: std::sync::Mutex<Option<Remote>> = std::sync::Mutex::new(None);

impl Connection {
    fn open(&self) -> Result<Remote> {
        let mut cached = REMOTE.lock().map_err(|_| invalid("session unavailable"))?;
        if let Some(remote) = cached
            .as_ref()
            .filter(|remote| remote.uses_credentials(&self.server, &self.password))
        {
            return Ok(remote.clone());
        }
        let remote = Remote::connect(&self.server, None)?;
        remote.use_password(self.password.clone())?;
        *cached = Some(remote.clone());
        Ok(remote)
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
        cursor: Option<String>,
    },
    Backup {
        connection: Connection,
        file: PathBuf,
        name: FileName,
        folder: FolderPath,
    },
    Open {
        connection: Connection,
        id: ContainerId,
        version: Option<VersionNum>,
        cache: PathBuf,
        #[serde(default)]
        export: bool,
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
        Request::Login { server, password } => login(&server, password),
        Request::Logout { connection } => {
            match connection.open()?.logout() {
                Ok(()) | Err(Error::Remote { status: 401, .. }) => {}
                Err(error) => return Err(error),
            }
            *REMOTE.lock().map_err(|_| invalid("session unavailable"))? = None;
            Ok(json!({}))
        }
        Request::Browse {
            connection,
            folder,
            cursor,
        } => Ok(serde_json::to_value(
            Catalog::open(&connection.open()?, None)?.list(&Filter {
                folder: Some(folder),
                cursor,
                ..Default::default()
            })?,
        )?),
        Request::Backup {
            connection,
            file,
            name,
            folder,
        } => backup::save(&connection.open()?, &file, &name, &folder),
        Request::Open {
            connection,
            id,
            version,
            cache,
            export,
        } => {
            let remote = connection.open()?;
            let (mut container, version) = remote.version(&id, version)?;
            container.latest_version = version.version;
            let bundle = Catalog::open(&remote, None)?.is_bundle(&container)?;
            if bundle && !export {
                return Ok(json!({"bundle": true}));
            }
            let mime = media_type(&version.name_at_commit);
            if mime.starts_with("video/") || mime.starts_with("audio/") {
                let (url, authorization) = remote.media_request(&id, version.version)?;
                Ok(
                    json!({ "url": url, "headers": { "Authorization": authorization }, "type": mime }),
                )
            } else {
                let path = cache::document(&remote, &cache, &id, &version)?;
                Ok(json!({ "path": path, "type": mime, "bundle": bundle }))
            }
        }
    }
}

fn login(endpoint: &str, password: String) -> Result<Value> {
    let remote = Remote::connect(endpoint, None)?;
    let session = remote.login(&password)?;
    let remote = Remote::connect(remote.endpoint(), Some(session.token))?;
    remote.use_password(password)?;
    let server = remote.endpoint().to_owned();
    *REMOTE.lock().map_err(|_| invalid("session unavailable"))? = Some(remote);
    Ok(json!({ "server": server }))
}
