//! The controls over local HTTP, for other programs and AIs in any language:
//! `POST /v1/tools/{name}` with the arguments as JSON, `GET /v1/tools` for the list.
//!
//! It only listens on this machine, and needs the token in the teams' folder. The Host and Origin
//! checks stop a web page you visit from reaching it through your browser.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use persephone_agents::control::{Desk, tools};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

/// Where the token is kept.
#[must_use]
pub fn token_path(desk: &Desk) -> PathBuf {
    desk.root.join("http-token")
}

/// The token, made the first time and kept readable only by you.
fn token(path: &Path) -> Result<String, String> {
    if let Ok(existing) = fs::read_to_string(path) {
        return Ok(existing.trim().to_owned());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    fs::write(path, &token).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    Ok(token)
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

/// Whether a Host or Origin names this machine on this port.
fn is_local(value: &str, port: u16) -> bool {
    let value = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value);
    value == format!("127.0.0.1:{port}") || value == format!("localhost:{port}")
}

/// Why a request is refused, if it is: the status and the reason.
fn refusal(request: &Request, token: &str, port: u16) -> Option<(u16, &'static str)> {
    if !header(request, "Host").is_some_and(|host| is_local(host, port)) {
        return Some((403, "this server only answers to its own local address"));
    }
    if header(request, "Origin").is_some_and(|origin| !is_local(origin, port)) {
        return Some((403, "this server doesn't answer web pages"));
    }
    let bearer = header(request, "Authorization").and_then(|value| value.strip_prefix("Bearer "));
    if bearer != Some(token) {
        return Some((
            401,
            "this needs `Authorization: Bearer <token>`, with the token in the teams' http-token file",
        ));
    }
    None
}

fn respond(request: Request, status: u16, body: &Value) {
    let response = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .expect("a valid header"),
        );
    let _ = request.respond(response);
}

fn handle(desk: &Desk, mut request: Request, token: &str, port: u16) {
    if let Some((status, reason)) = refusal(&request, token, port) {
        respond(request, status, &json!({"error": reason}));
        return;
    }
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or_default();
    match (request.method().clone(), path.strip_prefix("/v1/tools")) {
        (Method::Get, Some("" | "/")) => {
            let list: Vec<Value> = tools()
                .iter()
                .map(|tool| {
                    json!({"name": tool.name, "description": tool.what, "input_schema": tool.input_schema()})
                })
                .collect();
            respond(request, 200, &json!({"tools": list}));
        }
        (Method::Post, Some(rest)) if rest.len() > 1 => {
            let name = rest[1..].to_owned();
            let mut body = String::new();
            if request.as_reader().read_to_string(&mut body).is_err() {
                respond(
                    request,
                    400,
                    &json!({"error": "the body isn't readable text"}),
                );
                return;
            }
            let args = if body.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str::<Value>(&body)
            };
            let answer = match args {
                Ok(args) => desk.dispatch(&name, &args),
                Err(error) => Err(format!("the body isn't JSON: {error}")),
            };
            match answer {
                Ok(value) => respond(request, 200, &value),
                Err(error) => respond(request, 400, &json!({"error": error})),
            }
        }
        _ => respond(request, 404, &json!({"error": "try GET or POST /v1/tools"})),
    }
}

/// Listens on `port` of this machine (0 picks a free one) until the process ends, handling each
/// request on its own thread. Calls `ready` with the port it got.
///
/// # Errors
///
/// Returns a message if the port can't be had.
pub fn serve(desk: &Desk, port: u16, ready: impl FnOnce(u16)) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let port = server
        .server_addr()
        .to_ip()
        .map_or(port, |address| address.port());
    let token = Arc::new(token(&token_path(desk))?);
    let desk = Arc::new(desk.clone());
    ready(port);
    for request in server.incoming_requests() {
        let (desk, token) = (Arc::clone(&desk), Arc::clone(&token));
        std::thread::spawn(move || handle(&desk, request, &token, port));
    }
    Ok(())
}
