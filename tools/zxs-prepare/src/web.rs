use std::{
    io::{Cursor, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::{Result, build, disk, invalid};

const PAGE: &str = include_str!("../web/index.html");
const CSS: &str = include_str!("../web/style.css");
const JS: &str = include_str!("../web/app.js");
const JSON: &str = "application/json; charset=utf-8";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'";
const BODY_LIMIT: u64 = 4 * 1024 * 1024;

type Reply = Response<Cursor<Vec<u8>>>;

struct Local {
    host: String,
    origin: String,
    key: String,
}

pub fn run() -> Result<()> {
    let server = Server::http("127.0.0.1:0")?;
    let local = Local {
        host: server.server_addr().to_string(),
        origin: format!("http://{}", server.server_addr()),
        key: build::identifier(None)?,
    };
    let url = format!("{}/#{}", local.origin, local.key);
    println!("{url}");
    std::io::stdout().flush()?;
    if let Err(error) = open_browser(&url) {
        eprintln!("请在本机浏览器打开上方地址 · {error}");
    }
    for mut request in server.incoming_requests() {
        let response = local.route(&mut request).unwrap_or_else(|error| {
            let body = serde_json::json!({ "error": error.to_string() });
            reply(400, JSON, body.to_string())
        });
        if let Err(error) = request.respond(response) {
            eprintln!("浏览器连接已断开 · {error}");
        }
    }
    Ok(())
}

impl Local {
    fn route(&self, request: &mut Request) -> Result<Reply> {
        if header(request, "Host") != Some(self.host.as_str()) {
            return Ok(reply(403, "text/plain; charset=utf-8", "无效的本机地址"));
        }
        match (request.method(), request.url()) {
            (Method::Get, "/") => Ok(reply(200, "text/html; charset=utf-8", PAGE)),
            (Method::Get, "/style.css") => Ok(reply(200, "text/css; charset=utf-8", CSS)),
            (Method::Get, "/app.js") => Ok(reply(200, "text/javascript; charset=utf-8", JS)),
            (Method::Get, "/api.js") => Ok(reply(
                200,
                "text/javascript; charset=utf-8",
                include_str!("../web/api.js"),
            )),
            (Method::Post, "/api") => self.api(request),
            _ => Ok(reply(404, "text/plain; charset=utf-8", "页面不存在")),
        }
    }

    fn api(&self, request: &mut Request) -> Result<Reply> {
        if header(request, "Origin") != Some(self.origin.as_str())
            || header(request, "X-Zxs-Key") != Some(self.key.as_str())
        {
            return Ok(reply(
                403,
                JSON,
                r#"{"error":"请使用终端显示的本机链接。"}"#,
            ));
        }
        let mut body = Vec::new();
        request
            .as_reader()
            .take(BODY_LIMIT + 1)
            .read_to_end(&mut body)?;
        if body.len() as u64 > BODY_LIMIT {
            return Err(invalid("选择的项目过多"));
        }
        let action: Action = serde_json::from_slice(&body)?;
        match action {
            Action::Scan { root, within } => json(&disk::scan(&root, &within)?),
            Action::Init { root, kind } => json(&disk::init(&root, kind)?),
            Action::Prepare {
                root,
                mount_name,
                bundle,
            } => json(&disk::prepare(&root, mount_name.as_deref(), &bundle)?),
            Action::Resume { root } => json(&disk::resume(&root)?),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Scan {
        root: PathBuf,
        within: String,
    },
    Init {
        root: PathBuf,
        kind: disk::DiskKind,
    },
    Prepare {
        root: PathBuf,
        mount_name: Option<String>,
        bundle: Vec<String>,
    },
    Resume {
        root: PathBuf,
    },
}

fn json(value: &impl Serialize) -> Result<Reply> {
    Ok(reply(200, JSON, serde_json::to_vec(value)?))
}

fn reply(status: u16, mime: &str, body: impl Into<Vec<u8>>) -> Reply {
    let mut response = Response::from_data(body).with_status_code(StatusCode(status));
    for (field, value) in [
        ("Content-Type", mime),
        ("Cache-Control", "no-store"),
        ("Content-Security-Policy", CSP),
        ("X-Content-Type-Options", "nosniff"),
    ] {
        if let Ok(header) = Header::from_bytes(field, value) {
            response.add_header(header);
        }
    }
    response
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

fn open_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else if cfg!(target_os = "macos") {
        Command::new("open")
    } else {
        Command::new("xdg-open")
    };
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::Builder::new().spawn(move || {
        if !child.wait().is_ok_and(|status| status.success()) {
            eprintln!("无法打开默认浏览器，请在本机浏览器打开终端显示的地址。");
        }
    })?;
    Ok(())
}
