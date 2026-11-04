//! Command Code account sign-in and credentials.
//!
//! The browser flow matches the one used by the `cmd login` CLI (command-code 1.88): ReCodex
//! listens on a loopback port, opens `/studio/auth/cli` on commandcode.ai, and the Studio posts
//! a newly issued API key back to `http://127.0.0.1:<port>/callback` together with the state
//! token. The key is validated with `GET /alpha/whoami` and then used as a Bearer token for the
//! documented Provider API (`https://api.commandcode.ai/provider/v1`).
//!
//! The Studio endpoint and callback contract are not publicly documented; they were taken from
//! the CLI and may change.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const STUDIO_URL: &str = "https://commandcode.ai";
pub const API_URL: &str = "https://api.commandcode.ai";
pub const PROVIDER_PATH: &str = "/provider/v1";
pub const ENV_KEY: &str = "COMMAND_CODE_API_KEY";
const WHOAMI_PATH: &str = "/alpha/whoami";
const CALLBACK_PATH: &str = "/callback";
const COMPLETE_PATH: &str = "/callback/complete";
/// The CLI's callback port range; the Studio redirects to loopback ports in this range.
pub const FIRST_PORT: u16 = 5959;
const PORT_ATTEMPTS: u16 = 10;
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_BODY: usize = 10_000;
const ALLOWED_ORIGINS: [&str; 2] = ["https://commandcode.ai", "https://staging.commandcode.ai"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub api_key: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub key_name: String,
    #[serde(default)]
    pub authenticated_at: String,
}

/// Where the active credentials came from, in precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Environment,
    ReCodex,
    CommandCodeCli,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "$COMMAND_CODE_API_KEY",
            Self::ReCodex => "ReCodex sign-in",
            Self::CommandCodeCli => "~/.commandcode/auth.json",
        }
    }
}

/// Credential locations. ReCodex writes only its own store; the CLI file is read-only here.
#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
    pub cli_path: Option<PathBuf>,
    pub env_key: Option<String>,
}

impl Store {
    pub fn new(directory: &Path) -> Self {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        Self {
            path: directory.join("command-code-auth.json"),
            cli_path: home.map(|home| PathBuf::from(home).join(".commandcode").join("auth.json")),
            env_key: std::env::var(ENV_KEY)
                .ok()
                .map(|key| key.trim().to_owned())
                .filter(|key| !key.is_empty()),
        }
    }

    /// The active credentials: the environment, then ReCodex's sign-in, then the CLI's login.
    pub fn load(&self) -> Option<(Credentials, Source)> {
        if let Some(key) = &self.env_key {
            return Some((
                Credentials {
                    api_key: key.clone(),
                    ..Credentials::default()
                },
                Source::Environment,
            ));
        }
        if let Some(credentials) = read(&self.path) {
            return Some((credentials, Source::ReCodex));
        }
        self.cli_path
            .as_deref()
            .and_then(read)
            .map(|credentials| (credentials, Source::CommandCodeCli))
    }

    pub fn save(&self, credentials: &Credentials) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Invalid credential path")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut file, credentials).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        file.persist(&self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Forget ReCodex's sign-in. The key stays valid until it is revoked in Command Code Studio.
    pub fn clear(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

fn read(path: &Path) -> Option<Credentials> {
    let text = std::fs::read_to_string(path).ok()?;
    let credentials: Credentials = serde_json::from_str(&text).ok()?;
    (!credentials.api_key.is_empty()).then_some(credentials)
}

fn random_token() -> String {
    // 32 random bytes from two v4 UUIDs, hashed to remove the version bits, base64url-encoded.
    let seed = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    base64url(&Sha256::digest(seed.as_bytes()))
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(TABLE[((value >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(super) fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

/// A login attempt waiting for the browser callback.
pub struct PendingLogin {
    listener: TcpListener,
    state: String,
    pub auth_url: String,
}

impl PendingLogin {
    /// Bind the callback port and build the Studio URL to open in the browser.
    pub async fn start(studio_url: &str, first_port: u16) -> Result<Self, String> {
        let mut last = String::new();
        for offset in 0..PORT_ATTEMPTS {
            let port = first_port.saturating_add(offset);
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => {
                    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                    let state = random_token();
                    let callback = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
                    let auth_url = format!(
                        "{}/studio/auth/cli?callback={}&state={}&mode=redirect",
                        studio_url.trim_end_matches('/'),
                        percent_encode(&callback),
                        percent_encode(&state)
                    );
                    return Ok(Self {
                        listener,
                        state,
                        auth_url,
                    });
                }
                Err(e) => last = e.to_string(),
            }
            if first_port == 0 {
                break;
            }
        }
        Err(format!("No callback port is available for sign-in: {last}"))
    }

    /// Serve the callback until the Studio delivers credentials, the user denies access, or
    /// the attempt times out.
    pub async fn finish(self, timeout: Duration) -> Result<Credentials, String> {
        let state = self.state;
        let listener = self.listener;
        tokio::time::timeout(timeout, async move {
            let mut received: Option<Credentials> = None;
            loop {
                let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
                match serve(stream, &state, &mut received).await {
                    Ok(Some(result)) => return result,
                    Ok(None) | Err(_) => continue,
                }
            }
        })
        .await
        .map_err(|_| "Sign-in timed out. Start it again from ReCodex.".to_owned())?
    }
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: String,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| "timeout")?
            .map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("closed".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 16 * 1024 {
            return Err("headers too large".into());
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split(' ');
    let method = start.next().unwrap_or_default().to_owned();
    let target = start.next().unwrap_or("/").to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err("body too large".into());
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| "timeout")?
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Request {
        method,
        path: path.to_owned(),
        query: parse_query(query),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
    content_type: &str,
    body: &str,
) {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (key, value) in headers {
        response.push_str(&format!("{key}: {value}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(body);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn page(heading: &str, message: &str) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>ReCodex</title></head>\
         <body style=\"font-family:system-ui;padding:40px;text-align:center\"><h1>{}</h1><p>{}</p></body></html>",
        escape(heading),
        escape(message)
    )
}

/// Handle one callback connection. `Some` ends the login with its result.
async fn serve(
    mut stream: TcpStream,
    state: &str,
    received: &mut Option<Credentials>,
) -> Result<Option<Result<Credentials, String>>, String> {
    let request = read_request(&mut stream).await?;
    let origin = request
        .headers
        .get("origin")
        .filter(|origin| ALLOWED_ORIGINS.contains(&origin.as_str()))
        .cloned()
        .unwrap_or_else(|| ALLOWED_ORIGINS[0].to_owned());
    let mut cors = vec![
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS".into()),
        ("Access-Control-Allow-Headers", "Content-Type".into()),
    ];
    let html = "text/html; charset=utf-8";
    let json = "application/json";
    if request.method == "OPTIONS" {
        if request
            .headers
            .get("access-control-request-private-network")
            .is_some_and(|value| value == "true")
        {
            cors.push(("Access-Control-Allow-Private-Network", "true".into()));
        }
        respond(&mut stream, "204 No Content", &cors, json, "").await;
        return Ok(None);
    }
    if request.method == "GET" && request.path == COMPLETE_PATH {
        if request.query.get("state").map(String::as_str) != Some(state) {
            respond(
                &mut stream,
                "403 Forbidden",
                &cors,
                html,
                &page(
                    "Invalid state token",
                    "Return to ReCodex and start sign-in again.",
                ),
            )
            .await;
            return Ok(None);
        }
        return Ok(match received.take() {
            Some(credentials) => {
                respond(
                    &mut stream,
                    "200 OK",
                    &cors,
                    html,
                    &page(
                        "Signed in",
                        "You can close this window and return to ReCodex.",
                    ),
                )
                .await;
                Some(Ok(credentials))
            }
            None => {
                respond(
                    &mut stream,
                    "404 Not Found",
                    &cors,
                    html,
                    &page("Return to ReCodex", "Start sign-in from ReCodex."),
                )
                .await;
                None
            }
        });
    }
    if request.path != CALLBACK_PATH {
        respond(
            &mut stream,
            "404 Not Found",
            &cors,
            json,
            r#"{"success":false,"error":"Not found"}"#,
        )
        .await;
        return Ok(None);
    }
    if request.method != "POST" {
        respond(
            &mut stream,
            "405 Method Not Allowed",
            &cors,
            html,
            &page("Return to ReCodex", "Start sign-in from ReCodex."),
        )
        .await;
        return Ok(None);
    }
    let content_type = request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let fields: HashMap<String, String> = match content_type.as_str() {
        "application/x-www-form-urlencoded" => parse_query(&request.body),
        "application/json" => serde_json::from_str::<Value>(&request.body)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .map(|object| {
                object
                    .into_iter()
                    .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_owned())))
                    .collect()
            })
            .unwrap_or_default(),
        _ => {
            respond(
                &mut stream,
                "415 Unsupported Media Type",
                &cors,
                json,
                r#"{"success":false,"error":"Unsupported content type"}"#,
            )
            .await;
            return Ok(None);
        }
    };
    let form = content_type == "application/x-www-form-urlencoded";
    if fields.get("state").map(String::as_str) != Some(state) {
        respond(
            &mut stream,
            "403 Forbidden",
            &cors,
            if form { html } else { json },
            &if form {
                page(
                    "Invalid state token",
                    "Return to ReCodex and start sign-in again.",
                )
            } else {
                r#"{"success":false,"error":"Invalid state token"}"#.into()
            },
        )
        .await;
        return Ok(None);
    }
    if let Some(error) = fields.get("error") {
        let message = fields.get("error_description").cloned().unwrap_or_else(|| {
            if error == "access_denied" {
                "Command Code sign-in was denied".into()
            } else {
                error.clone()
            }
        });
        respond(
            &mut stream,
            "200 OK",
            &cors,
            if form { html } else { json },
            &if form {
                page("Sign-in was not completed", &message)
            } else {
                r#"{"success":true}"#.into()
            },
        )
        .await;
        return Ok(Some(Err(message)));
    }
    let field = |key: &str| fields.get(key).filter(|value| !value.is_empty()).cloned();
    let (Some(api_key), Some(user_id), Some(user_name), Some(key_name)) = (
        field("apiKey"),
        field("userId"),
        field("userName"),
        field("keyName"),
    ) else {
        respond(
            &mut stream,
            "400 Bad Request",
            &cors,
            if form { html } else { json },
            &if form {
                page(
                    "Invalid request",
                    "Required parameters are missing. Start sign-in again from ReCodex.",
                )
            } else {
                r#"{"success":false,"error":"Missing required fields"}"#.into()
            },
        )
        .await;
        return Ok(None);
    };
    let credentials = Credentials {
        api_key,
        user_id,
        user_name,
        key_name,
        authenticated_at: timestamp_now(),
    };
    if form {
        // The browser lands on the completion page, which ends the attempt.
        *received = Some(credentials);
        respond(
            &mut stream,
            "303 See Other",
            &[(
                "Location",
                format!("{COMPLETE_PATH}?state={}", percent_encode(state)),
            )],
            html,
            "",
        )
        .await;
        Ok(None)
    } else {
        respond(&mut stream, "200 OK", &cors, json, r#"{"success":true}"#).await;
        Ok(Some(Ok(credentials)))
    }
}

fn timestamp_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    // RFC 3339 in UTC without pulling in a date library.
    let days = seconds / 86_400;
    let (mut year, mut day) = (1970i64, days as i64);
    loop {
        let length = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
            366
        } else {
            365
        };
        if day < length {
            break;
        }
        day -= length;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while day >= months[month] {
        day -= months[month];
        month += 1;
    }
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        month + 1,
        day + 1,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Check a key against the account API. Returns the signed-in user on success.
pub async fn validate(api_url: &str, api_key: &str) -> Result<Value, String> {
    let response = reqwest::Client::new()
        .get(format!("{}{WHOAMI_PATH}", api_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Command Code could not be reached: {e}"))?;
    match response.status().as_u16() {
        401 => Err("Command Code rejected the API key".into()),
        status if !(200..300).contains(&status) => Err(format!(
            "Command Code account check failed with HTTP {status}"
        )),
        _ => response
            .json()
            .await
            .map_err(|e| format!("Command Code returned an invalid account response: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send(port: u16, request: String) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    fn form_post(fields: &str) -> String {
        format!(
            "POST /callback HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://commandcode.ai\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{fields}",
            fields.len()
        )
    }

    fn port_and_state(url: &str) -> (u16, String) {
        let query = parse_query(url.split_once('?').unwrap().1);
        let callback = &query["callback"];
        let port = callback
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches("/callback")
            .parse()
            .unwrap();
        assert_eq!(query["mode"], "redirect");
        (port, query["state"].clone())
    }

    #[tokio::test]
    async fn browser_callback_delivers_credentials_after_the_completion_page() {
        let login = PendingLogin::start("https://commandcode.ai", 0)
            .await
            .unwrap();
        assert!(login.auth_url.starts_with(
            "https://commandcode.ai/studio/auth/cli?callback=http%3A%2F%2F127.0.0.1%3A"
        ));
        let (port, state) = port_and_state(&login.auth_url);
        let finished = tokio::spawn(login.finish(Duration::from_secs(10)));

        let preflight = send(port, "OPTIONS /callback HTTP/1.1\r\nOrigin: https://commandcode.ai\r\nAccess-Control-Request-Private-Network: true\r\n\r\n".into()).await;
        assert!(preflight.starts_with("HTTP/1.1 204"));
        assert!(preflight.contains("Access-Control-Allow-Private-Network: true"));
        let forged = send(
            port,
            form_post("apiKey=k&state=wrong&userId=u&userName=n&keyName=c"),
        )
        .await;
        assert!(forged.starts_with("HTTP/1.1 403"));
        let posted = send(
            port,
            form_post(&format!(
                "apiKey=user_key%2B1&state={}&userId=u1&userName=Ada+L&keyName=cli",
                percent_encode(&state)
            )),
        )
        .await;
        assert!(posted.starts_with("HTTP/1.1 303"));
        let location = posted
            .lines()
            .find_map(|line| line.strip_prefix("Location: "))
            .unwrap()
            .to_owned();
        let complete = send(port, format!("GET {location} HTTP/1.1\r\n\r\n")).await;
        assert!(complete.starts_with("HTTP/1.1 200"));
        let credentials = finished.await.unwrap().unwrap();
        assert_eq!(credentials.api_key, "user_key+1");
        assert_eq!(credentials.user_name, "Ada L");
        assert!(credentials.authenticated_at.ends_with('Z'));
    }

    #[tokio::test]
    async fn denial_and_timeout_end_the_attempt() {
        let login = PendingLogin::start("https://commandcode.ai", 0)
            .await
            .unwrap();
        let (port, state) = port_and_state(&login.auth_url);
        let finished = tokio::spawn(login.finish(Duration::from_secs(10)));
        let body = format!(r#"{{"state":"{state}","error":"access_denied"}}"#);
        send(
            port,
            format!(
                "POST /callback HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert_eq!(
            finished.await.unwrap().unwrap_err(),
            "Command Code sign-in was denied"
        );
        let login = PendingLogin::start("https://commandcode.ai", 0)
            .await
            .unwrap();
        assert!(login.finish(Duration::from_millis(50)).await.is_err());
    }

    #[test]
    fn credentials_prefer_the_environment_then_recodex_then_the_cli() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("cli.json");
        std::fs::write(&cli, r#"{"apiKey":"cli-key","userName":"cli","extra":1}"#).unwrap();
        let mut store = Store {
            path: directory.path().join("recodex.json"),
            cli_path: Some(cli),
            env_key: None,
        };
        assert_eq!(store.load().unwrap().1, Source::CommandCodeCli);
        store
            .save(&Credentials {
                api_key: "recodex-key".into(),
                ..Credentials::default()
            })
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(store.load().unwrap().0.api_key, "recodex-key");
        store.env_key = Some("env-key".into());
        assert_eq!(
            store.load().unwrap(),
            (
                Credentials {
                    api_key: "env-key".into(),
                    ..Credentials::default()
                },
                Source::Environment
            )
        );
        store.env_key = None;
        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load().unwrap().0.api_key, "cli-key");
    }

    #[test]
    fn encodings_round_trip() {
        assert_eq!(base64url(&[0xfb, 0xff, 0x00]), "-_8A");
        assert_eq!(random_token().len(), 43);
        assert_eq!(percent_decode(&percent_encode("a b+c/é")), "a b+c/é");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
