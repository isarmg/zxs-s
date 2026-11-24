mod actions;
mod media;
mod session;

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use zxs_core::{Error, METADATA_LIMIT, Profile, Remote, Result, invalid};

use self::session::Access;

const HTML: &str = include_str!("ui/index.html");
const CSS: &str = include_str!("ui/style.css");
const JS: &str = include_str!("ui/app.js");
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

struct Ui {
    root: PathBuf,
    address: SocketAddr,
    access: Access,
    auth: Mutex<Option<(Profile, Remote)>>,
}

pub fn run(root: &Path) -> Result<()> {
    let server = Arc::new(Server::http("127.0.0.1:0").map_err(|error| invalid(error.to_string()))?);
    let address = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| invalid("loopback address required"))?;
    let (access, secret) = Access::new(address.port())?;
    let state = Arc::new(Ui {
        root: fs::canonicalize(root)?,
        address,
        access,
        auth: Mutex::new(None),
    });
    let url = format!("http://{address}/#{secret}");
    println!("{url}");
    if let Err(error) = open_target(Path::new(&url)) {
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
    let response = route(state, &mut request).unwrap_or_else(|error| error_response(&error));
    request.respond(response)?;
    Ok(())
}

type Reply = Response<Box<dyn std::io::Read + Send>>;

fn route(state: &Ui, request: &mut Request) -> Result<Reply> {
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
        (Method::Get, "/style.css") => Ok(static_file(CSS, "text/css; charset=utf-8")),
        (Method::Get, "/app.js") => Ok(static_file(JS, "text/javascript; charset=utf-8")),
        (Method::Post, "/access") => state.access.authorize(request),
        _ if !state.access.valid(request) => Err(Error::Remote {
            status: 401,
            message: "open the one-time URL from zxs-cli ui".into(),
        }),
        (Method::Post, "/api") => actions::api(state, request),
        (Method::Get | Method::Head, path) if path.starts_with("/media/") => {
            media::serve(state, request)
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
        if state.is_none()
            && let Some(profile) = Profile::load()?
        {
            let remote = profile.remote()?;
            *state = Some((profile, remote));
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
    json_response(&serde_json::json!({ "message": error.to_string(),
        "remoteConflict": matches!(error, Error::Remote { status: 409, .. })
    }))
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
