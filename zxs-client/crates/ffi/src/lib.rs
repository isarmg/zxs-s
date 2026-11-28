mod backup;
mod cache;
mod files;
mod upload;

use std::{
    ffi::{CString, c_char, c_void},
    panic::AssertUnwindSafe,
    ptr, slice,
    sync::Arc,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, DownloadCancellation, Error, Filter, FolderPath, METADATA_LIMIT, Remote, Result,
    TransferDirection, TransferObserver, TransferProgress, invalid,
};

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Connection {
    server: String,
    password: String,
}

static REMOTE: std::sync::Mutex<Option<Remote>> = std::sync::Mutex::new(None);

struct Transfer {
    progress: Option<TransferObserver>,
    cancelled: Option<DownloadCancellation>,
}

impl Connection {
    fn transfer(&self, transfer: Transfer) -> Result<Remote> {
        Ok(self
            .open()?
            .with_progress(transfer.progress)
            .with_download_cancellation(transfer.cancelled))
    }

    fn open(&self) -> Result<Remote> {
        let mut cached = REMOTE.lock().map_err(|_| invalid("session unavailable"))?;
        if let Some(remote) = cached
            .as_ref()
            .filter(|remote| remote.uses_credentials(&self.server, &self.password))
        {
            return Ok(remote.clone());
        }
        let remote = Remote::connect(&self.server)?;
        remote.use_password(self.password.clone())?;
        *cached = Some(remote.clone());
        Ok(remote)
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Login(Connection),
    Logout {
        connection: Connection,
    },
    Browse {
        connection: Connection,
        folder: FolderPath,
        cursor: Option<String>,
    },
    BrowseBundle(files::BrowseBundle),
    OpenBundleMember(files::OpenMember),
    CommitFile(upload::CommitFile),
    Open(files::OpenFile),
}

type ProgressCallback = unsafe extern "C" fn(*mut c_void, u8, u64, u64, u64);
type CancelCallback = unsafe extern "C" fn(*mut c_void) -> bool;

/// Dispatches UTF-8 JSON with optional callbacks and returns a NUL-terminated UTF-8 response.
///
/// # Safety
/// The request, callbacks and context must stay valid for this call. The callbacks must
/// not unwind. Once cancellation is requested it must remain set. Release the result
/// with `zxs_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zxs_call_with_transfer(
    request: *const u8,
    length: usize,
    callback: Option<ProgressCallback>,
    cancelled: Option<CancelCallback>,
    context: *mut c_void,
) -> *mut c_char {
    let context = context.expose_provenance();
    let progress = callback.map(|callback| {
        Arc::new(move |progress: TransferProgress| {
            let direction = u8::from(matches!(progress.direction, TransferDirection::Download));
            // The per-request remote is dropped before returning to the foreign caller.
            unsafe {
                callback(
                    ptr::with_exposed_provenance_mut(context),
                    direction,
                    progress.transferred,
                    progress.total,
                    progress.bytes_per_second,
                );
            }
        }) as TransferObserver
    });
    let cancelled = cancelled.map(|callback| {
        Arc::new(move || unsafe { callback(ptr::with_exposed_provenance_mut(context)) })
            as DownloadCancellation
    });
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if request.is_null() || length == 0 || length > METADATA_LIMIT {
            return Err(invalid("invalid C ABI request"));
        }
        // The foreign caller provides the buffer and keeps it alive for this call.
        let bytes = unsafe { slice::from_raw_parts(request, length) };
        run(
            serde_json::from_slice(bytes)?,
            Transfer {
                progress,
                cancelled,
            },
        )
    }));
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

/// Releases a response allocated by `zxs_call_with_transfer`.
///
/// # Safety
///
/// value must be null or an unfreed response pointer from `zxs_call_with_transfer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zxs_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

fn run(request: Request, transfer: Transfer) -> Result<Value> {
    match request {
        Request::Login(connection) => login(&connection.server, connection.password),
        Request::Logout { connection } => {
            files::close_bundle()?;
            let remote = connection.open()?;
            *REMOTE.lock().map_err(|_| invalid("session unavailable"))? = None;
            remote.logout()?;
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
            let page = serde_json::to_value(catalog.list(&filter)?)?;
            files::close_bundle().map(|()| page)
        }
        Request::BrowseBundle(request) => request.run(transfer),
        Request::OpenBundleMember(request) => request.run(transfer),
        Request::CommitFile(request) => request.run(transfer.progress),
        Request::Open(request) => request.run(transfer),
    }
}

fn login(endpoint: &str, password: String) -> Result<Value> {
    let remote = Remote::connect(endpoint)?;
    remote.use_password(password)?;
    remote.authenticate()?;
    let server = remote.endpoint().to_owned();
    *REMOTE.lock().map_err(|_| invalid("session unavailable"))? = Some(remote);
    Ok(json!({ "server": server }))
}
