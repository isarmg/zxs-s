mod backup;
mod cache;
mod files;

use std::{
    ffi::{CString, c_char},
    ptr, slice,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, ContainerId, Error, Filter, FolderPath, METADATA_LIMIT, Remote, Result, invalid,
};

#[derive(Clone, Deserialize, PartialEq, Eq)]
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
    Folders {
        connection: Connection,
        folder: FolderPath,
    },
    Lookup {
        connection: Connection,
        ids: Vec<ContainerId>,
    },
    BrowseBundle(files::BrowseBundle),
    OpenBundleMember(files::OpenMember),
    Download(files::Download),
    Backup(backup::Backup),
    Open(files::OpenFile),
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
        Ok(Err(error)) => json!({
            "ok": false,
            "status": error.status(),
            "error": match &error {
                Error::PasswordRejected(_) => "服务器口令错误，请重新输入".into(),
                Error::Http(_) => format!("服务器连接失败 · {error}"),
                _ => error.to_string(),
            },
            "password_rejected": matches!(error, Error::PasswordRejected(_)),
        }),
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
            files::close_bundle()?;
            backup::close_batch()?;
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
        } => {
            let remote = connection.open()?;
            let catalog = Catalog::open(&remote, None)?;
            let filter = Filter {
                folder: Some(folder),
                cursor,
                ..Default::default()
            };
            Ok(serde_json::to_value(catalog.list(&filter)?)?)
        }
        Request::BrowseBundle(request) => request.run(),
        Request::OpenBundleMember(request) => request.run(),
        Request::Folders { connection, folder } => {
            Ok(serde_json::to_value(connection.open()?.folders(&folder)?)?)
        }
        Request::Lookup { connection, ids } => {
            let containers: Vec<_> = connection
                .open()?
                .lookup_containers(ids.iter())?
                .into_values()
                .collect();
            Ok(json!({ "containers": containers }))
        }
        Request::Download(request) => request.run(),
        Request::Backup(request) => request.run(),
        Request::Open(request) => request.run(),
    }
}

fn login(endpoint: &str, password: String) -> Result<Value> {
    let remote = Remote::connect(endpoint, None)?;
    remote.use_password(password)?;
    remote.authenticate()?;
    let server = remote.endpoint().to_owned();
    *REMOTE.lock().map_err(|_| invalid("session unavailable"))? = Some(remote);
    Ok(json!({ "server": server }))
}
