mod actions;
mod media;
mod progress;
mod session;

use std::{
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, RwLock},
};

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use zxs_core::{Error, METADATA_LIMIT, Profile, Remote, Result, invalid};

use self::session::Access;

const HTML: &str = include_str!("ui/index.html");
const CSS_MIME: &str = "text/css; charset=utf-8";
const JS_MIME: &str = "text/javascript; charset=utf-8";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

struct Ui {
    root: RwLock<PathBuf>,
    address: SocketAddr,
    access: Access,
    auth: Mutex<Option<(Profile, Remote)>>,
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
    let mut job = None;
    let download = request.url().starts_with("/media/");
    let response = route(state, &mut request, &mut job);
    let failure = job
        .as_ref()
        .and_then(|_| response.as_ref().err().map(ToString::to_string));
    let sent = request.respond(response.unwrap_or_else(|error| error_response(&error)));
    if let Some(job) = job {
        job.finish(
            failure.or_else(|| sent.as_ref().err().map(ToString::to_string)),
            download,
        );
    }
    sent?;
    Ok(())
}

type Reply = Response<Box<dyn std::io::Read + Send>>;

fn route(state: &Ui, request: &mut Request, job: &mut Option<progress::Job>) -> Result<Reply> {
    if read_header(request, "host") != Some(state.address.to_string().as_str()) {
        return Err(Error::Remote {
            status: 403,
            message: "invalid loopback host".into(),
        });
    }
    if request.method() == &Method::Post
        && read_header(request, "origin") != Some(format!("http://{}", state.address).as_str())
    {
        return Err(Error::Remote {
            status: 403,
            message: "invalid loopback origin".into(),
        });
    }
    match (request.method(), request.url()) {
        (Method::Get, "/") => Ok(static_file(HTML, "text/html; charset=utf-8")),
        (Method::Get, "/style.css") => Ok(static_file(include_str!("ui/style.css"), CSS_MIME)),
        (Method::Get, "/components.css") => {
            Ok(static_file(include_str!("ui/components.css"), CSS_MIME))
        }
        (Method::Get, "/app.js") => Ok(static_file(include_str!("ui/app.js"), JS_MIME)),
        (Method::Get, "/api.js") => Ok(static_file(include_str!("ui/api.js"), JS_MIME)),
        (Method::Get, "/progress.js") => Ok(static_file(include_str!("ui/progress.js"), JS_MIME)),
        (Method::Get, "/view.js") => Ok(static_file(include_str!("ui/view.js"), JS_MIME)),
        (Method::Get, "/widgets.js") => Ok(static_file(include_str!("ui/widgets.js"), JS_MIME)),
        (Method::Post, "/access") => state.access.authorize(request),
        _ if !state.access.valid(request) => Err(Error::Remote {
            status: 401,
            message: "open the one-time URL from zxs-cli ui".into(),
        }),
        (Method::Post, "/api") => {
            *job = state
                .transfers
                .start(read_header(request, "x-zxs-transfer"))?;
            actions::api(state, request, job.as_ref())
        }
        (Method::Get, path) if path.starts_with("/transfer/") => state.transfers.read(&path[10..]),
        (Method::Get | Method::Head, path) if path.starts_with("/media/") => {
            media::serve(state, request, job)
        }
        _ => Err(Error::Remote {
            status: 404,
            message: "route not found".into(),
        }),
    }
}

impl Ui {
    fn remote(&self) -> Result<Remote> {
        let mut state = self
            .auth
            .lock()
            .map_err(|error| invalid(error.to_string()))?;
        let current = Profile::load()?;
        if state.as_ref().map(|(profile, _)| profile) != current.as_ref() {
            if let Some((_, previous)) = state.take() {
                let _ = previous.logout();
            }
            if let Some(profile) = current {
                let remote = profile.remote()?;
                *state = Some((profile, remote));
            }
        }
        state
            .as_ref()
            .map(|(_, remote)| remote.clone())
            .ok_or_else(|| Error::Remote {
                status: 401,
                message: "connect to a ZXS server".into(),
            })
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

fn static_file(data: &str, mime: &str) -> Reply {
    boxed(Response::from_string(data), mime)
}

fn json_response(value: &serde_json::Value) -> Reply {
    static_file(&value.to_string(), "application/json; charset=utf-8")
}

fn error_response(error: &Error) -> Reply {
    let code = match error {
        Error::CommitConflict(_) => Some("commit_conflict"),
        Error::CheckoutConflict => Some("checkout_conflict"),
        Error::CheckoutInterrupted => Some("checkout_interrupted"),
        _ => None,
    };
    json_response(&serde_json::json!({ "message": error.to_string(), "code": code }))
        .with_status_code(StatusCode(error.status()))
}

fn boxed(response: Response<std::io::Cursor<Vec<u8>>>, mime: &str) -> Reply {
    let mut response = response.boxed();
    for (name, value) in [
        ("Content-Type", mime),
        ("Content-Security-Policy", CSP),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("Cache-Control", "no-store"),
    ] {
        if let Ok(header) = header(name, value) {
            response.add_header(header);
        }
    }
    response
}
