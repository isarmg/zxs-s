mod actions;
mod media;
mod progress;
mod session;

use std::{
    fs,
    hash::{BuildHasher, RandomState},
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, RwLock},
};

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use zxs_core::{Error, METADATA_LIMIT, Profile, Remote, Result, invalid};

use self::session::Access;

const CSS_MIME: &str = "text/css; charset=utf-8";
const JS_MIME: &str = "text/javascript; charset=utf-8";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "/",
        "text/html; charset=utf-8",
        include_bytes!("ui/index.html"),
    ),
    (
        "/app-icon.png",
        "image/png",
        include_bytes!("ui/app-icon.png"),
    ),
    ("/style.css", CSS_MIME, include_bytes!("ui/style.css")),
    (
        "/components.css",
        CSS_MIME,
        include_bytes!("ui/components.css"),
    ),
    ("/app.js", JS_MIME, include_bytes!("ui/app.js")),
    ("/api.js", JS_MIME, include_bytes!("ui/api.js")),
    ("/progress.js", JS_MIME, include_bytes!("ui/progress.js")),
    ("/view.js", JS_MIME, include_bytes!("ui/view.js")),
    ("/widgets.js", JS_MIME, include_bytes!("ui/widgets.js")),
];

struct Ui {
    root: RwLock<PathBuf>,
    address: SocketAddr,
    access: Access,
    auth: Mutex<Option<(Profile, Remote)>>,
    targets: RandomState,
    transfers: progress::Transfers,
}

pub fn run(root: &Path) -> Result<()> {
    let root = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => std::path::absolute(root)?,
        Err(error) => return Err(error.into()),
    };
    let server = Arc::new(Server::http("127.0.0.1:0").map_err(|error| invalid(error.to_string()))?);
    let address = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| invalid("loopback address required"))?;
    let (access, secret) = Access::new(address.port())?;
    let state = Arc::new(Ui {
        root: RwLock::new(root),
        address,
        access,
        auth: Mutex::new(None),
        targets: RandomState::new(),
        transfers: progress::Transfers::default(),
    });
    let url = format!("http://{address}/#{secret}");
    println!("{url}");
    if let Err(error) = open_browser(&url) {
        eprintln!("Could not open the browser: {error}");
    }
    std::thread::scope(|scope| {
        for _ in 0..12 {
            let state = Arc::clone(&state);
            let server = Arc::clone(&server);
            scope.spawn(move || {
                for incoming in server.incoming_requests() {
                    if let Err(error) = dispatch(&state, incoming) {
                        eprintln!("local UI: {error}");
                    }
                }
            });
        }
    });
    Ok(())
}

fn dispatch(state: &Ui, mut request: Request) -> Result<()> {
    let mut response = route(state, &mut request).unwrap_or_else(|error| error_response(&error));
    for (name, value) in [
        ("Content-Security-Policy", CSP),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("Cache-Control", "private, no-store"),
    ] {
        response.add_header(header(name, value)?);
    }
    request.respond(response)?;
    Ok(())
}

type Reply = Response<Box<dyn std::io::Read + Send>>;

fn route(state: &Ui, request: &mut Request) -> Result<Reply> {
    if read_header(request, "host") != Some(state.address.to_string().as_str()) {
        return Err(http_error(403, "invalid loopback host"));
    }
    if request.method() == &Method::Post
        && read_header(request, "origin") != Some(format!("http://{}", state.address).as_str())
    {
        return Err(http_error(403, "invalid loopback origin"));
    }
    if request.method() == &Method::Get
        && let Some((_, mime, data)) = ASSETS.iter().find(|(url, ..)| *url == request.url())
    {
        return Ok(file(*data, mime));
    }
    match (request.method(), request.url()) {
        (Method::Post, "/access") => state.access.authorize(request),
        _ if !state.access.valid(request) => {
            Err(http_error(401, "open the one-time URL from zxs-cli ui"))
        }
        (Method::Post, "/api") => {
            let id = read_header(request, "x-zxs-transfer").map(str::to_owned);
            let observer = state.transfers.start(id.as_deref())?;
            let response = actions::api(state, request, observer);
            state.transfers.finish(id.as_deref());
            response
        }
        (Method::Get, path) if path.starts_with("/transfer/") => state.transfers.read(&path[10..]),
        (Method::Get | Method::Head, path) if path.starts_with("/media/") => {
            media::serve(state, request)
        }
        _ => Err(http_error(404, "route not found")),
    }
}

impl Ui {
    fn target(&self, root: &Path, profile: Option<&Profile>) -> String {
        let server = profile.map(|profile| profile.endpoint.as_str());
        format!("{:016x}", self.targets.hash_one((root, server)))
    }

    fn targeted_profile(&self, root: &Path, target: Option<&str>) -> Result<Option<Profile>> {
        let profile = Profile::load()?;
        if target != Some(self.target(root, profile.as_ref()).as_str()) {
            return Err(http_error(409, "工作区或服务器已切换，请刷新后重试。"));
        }
        Ok(profile)
    }

    fn remote(&self, profile: Option<Profile>) -> Result<Remote> {
        let mut state = self
            .auth
            .lock()
            .map_err(|error| invalid(error.to_string()))?;
        if state.as_ref().map(|(profile, _)| profile) != profile.as_ref() {
            if let Some((_, previous)) = state.take() {
                let _ = previous.logout();
            }
            if let Some(profile) = profile {
                let remote = profile.remote()?;
                *state = Some((profile, remote));
            }
        }
        state
            .as_ref()
            .map(|(_, remote)| remote.clone())
            .ok_or_else(|| http_error(401, "connect to a ZXS server"))
    }
}

fn open_browser(url: &str) -> Result<()> {
    if cfg!(windows) {
        Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .stdin(Stdio::null())
            .spawn()?;
        Ok(())
    } else {
        open_target(Path::new(url))
    }
}

fn open_target(target: &Path) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        Command::new("explorer.exe")
    } else {
        Command::new("xdg-open")
    };
    command.arg(target).stdin(Stdio::null()).spawn()?;
    Ok(())
}

fn read_header<'a>(request: &'a Request, key: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(key))
        .map(|header| header.value.as_str())
}

fn header(name: &str, value: &str) -> Result<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .map_err(|()| invalid("invalid response header"))
}

fn json_input<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T> {
    use std::io::Read;
    if read_header(request, "content-type") != Some("application/json") {
        return Err(invalid("application/json required"));
    }
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(METADATA_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > METADATA_LIMIT {
        return Err(invalid("request body exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))
}

fn file(data: impl Into<Vec<u8>>, mime: &str) -> Reply {
    let mut response = Response::from_data(data);
    if let Ok(content_type) = header("Content-Type", mime) {
        response.add_header(content_type);
    }
    response.boxed()
}

fn json_response(value: &serde_json::Value) -> Reply {
    file(value.to_string(), "application/json; charset=utf-8")
}

fn error_response(error: &Error) -> Reply {
    let (code, expected) = match error {
        Error::CommitConflict(_, target) => (Some("commit_conflict"), Some(target)),
        Error::CheckoutConflict => (Some("checkout_conflict"), None),
        Error::CheckoutInterrupted => (Some("checkout_interrupted"), None),
        _ => (None, None),
    };
    json_response(&serde_json::json!({ "message": error.to_string(), "code": code, "expected_container": expected }))
        .with_status_code(StatusCode(error.status()))
}

fn http_error(status: u16, message: &str) -> Error {
    Error::Remote {
        status,
        message: message.into(),
    }
}
