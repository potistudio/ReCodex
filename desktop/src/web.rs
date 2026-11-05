//! Web UI hosting. Serves the desktop UI build over HTTPS with the shared LAN identity and
//! bridges a browser session onto `Host` through the same shared-project boundary that LAN
//! peers use (`Host::request(true, …)`). There is no application authentication: anyone who
//! can reach the listener may drive the host's model provider for shared projects, so enable
//! it only on trusted networks.
use crate::{host::Host, lan};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Semaphore},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    tungstenite::{handshake::derive_accept_key, protocol::Role, Message},
    WebSocketStream,
};

const TIMEOUT: Duration = Duration::from_secs(65);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER: usize = 8 * 1024;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Registered UI build locations. The desktop bundle embeds its assets, so this only
/// observes well-known folders; `RECODEX_WEB_UI_DIR` overrides the search entirely.
fn ui_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("RECODEX_WEB_UI_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(base) = exe.parent() {
            let mut directory = Some(PathBuf::from(base));
            for _ in 0..4 {
                let Some(current) = directory else { break };
                candidates.push(current.join("build"));
                directory = current.parent().map(Path::to_path_buf);
            }
        }
    }
    if let Ok(directory) = std::env::current_dir() {
        candidates.push(directory.join("build"));
        candidates.push(directory.join("../build"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn resolve_ui() -> Option<PathBuf> {
    ui_candidates()
        .into_iter()
        .find(|dir| dir.join("index.html").is_file())
}

pub struct WebServer {
    address: String,
    stop: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl WebServer {
    pub fn status(&self) -> Value {
        json!({ "address": self.address })
    }
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for WebServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}

/// Start serving the Web UI. Web sessions follow the LAN shared-project list, so enable
/// hosting together with the Web UI for access; otherwise sessions see no projects.
pub async fn serve(
    host: Arc<Host>,
    address: &str,
    ui: Option<PathBuf>,
) -> Result<Arc<WebServer>, String> {
    let ui = match ui {
        Some(dir) => dir,
        None => resolve_ui().ok_or(
            "No Web UI build found. Run `pnpm build` first or set RECODEX_WEB_UI_DIR to a directory containing index.html.",
        )?,
    };
    if !ui.join("index.html").is_file() {
        return Err("The Web UI directory has no index.html".into());
    }
    let ui = fs::canonicalize(&ui).map_err(error)?;
    let tls = lan::identity_tls(&host.directory)?;
    let socket = TcpListener::bind(address).await.map_err(error)?;
    let address = socket.local_addr().map_err(error)?.to_string();
    let (stop, stopped) = watch::channel(false);
    let server = Arc::new(WebServer {
        address: address.clone(),
        stop,
        task: StdMutex::new(None),
    });
    let task = tokio::spawn(async move {
        let mut stopped = stopped;
        let permits = Arc::new(Semaphore::new(32));
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                result = socket.accept() => {
                    let Ok((stream, _)) = result else { break };
                    let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                    let host = host.clone();
                    let ui = ui.clone();
                    let tls = tls.clone();
                    let stopped = stopped.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = connection(stream, tls, host, ui, stopped).await;
                    });
                }
            }
        }
    });
    *server.task.lock().unwrap() = Some(task);
    Ok(server)
}

/// One keep-alive HTTP session; the WebSocket branch takes over the stream and returns.
async fn connection(
    stream: TcpStream,
    tls: TlsAcceptor,
    host: Arc<Host>,
    ui: PathBuf,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(stream))
        .await
        .map_err(|_| "TLS handshake timed out".to_string())?
        .map_err(error)?;
    loop {
        if *stopped.borrow() {
            let _ = stream.shutdown().await;
            return Ok(());
        }
        tokio::select! {
            _ = stopped.changed() => {
                let _ = stream.shutdown().await;
                return Ok(());
            }
            head = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_head(&mut stream)) => {
                let head = head.map_err(|_| "HTTP request timed out".to_string())??;
                let request = match Request::parse(&head) {
                    Ok(request) => request,
                    Err(message) => {
                        write_response(&mut stream, 400, "Bad Request", "text/plain; charset=utf-8", message.as_bytes(), false).await?;
                        let _ = stream.shutdown().await;
                        return Ok(());
                    }
                };
                let path = request.path.split('?').next().unwrap_or_default().trim_matches('/').to_owned();
                if path == "api" {
                    let result = websocket(&mut stream, &request, host, stopped.clone()).await;
                    let _ = stream.shutdown().await;
                    return result;
                }
                if path == "api/info" {
                    write_response(&mut stream, 200, "OK", "application/json", br#"{"web":true}"#, request.keep_alive).await?;
                } else if request.method == "GET" {
                    static_file(&mut stream, &ui, &path, request.keep_alive).await?;
                } else {
                    write_response(&mut stream, 405, "Method Not Allowed", "text/plain; charset=utf-8", b"GET only", false).await?;
                }
                if !request.keep_alive {
                    let _ = stream.shutdown().await;
                    return Ok(());
                }
            }
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    keep_alive: bool,
}
impl Request {
    fn parse(head: &str) -> Result<Self, String> {
        let mut lines = head.split("\r\n");
        let first = lines.next().unwrap_or_default();
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        if method.is_empty()
            || path.is_empty()
            || !parts
                .next()
                .is_some_and(|version| version.starts_with("HTTP/1."))
        {
            return Err("Malformed HTTP request".into());
        }
        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once(':').ok_or("Malformed HTTP header")?;
            if value.len() > 1024 || headers.len() > 80 {
                return Err("Malformed HTTP header".into());
            }
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        let keep_alive = headers.get("connection").is_none_or(|value| {
            !value
                .to_ascii_lowercase()
                .split(',')
                .any(|part| part.trim() == "close")
        });
        Ok(Self {
            method,
            path,
            headers,
            keep_alive,
        })
    }
}

async fn read_head<S>(stream: &mut S) -> Result<String, String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    loop {
        if bytes.len() >= MAX_HEADER {
            return Err("HTTP request head is too large".into());
        }
        let byte = stream.read_u8().await.map_err(error)?;
        bytes.push(byte);
        if byte == b'\n' && bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| "HTTP request head is not UTF-8".into())
}

fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" | "log" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

async fn static_file<S>(
    stream: &mut S,
    ui: &Path,
    path: &str,
    keep_alive: bool,
) -> Result<(), String>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let target = sanitize(ui, path);
    match target.as_ref().and_then(|path| fs::read(path).ok()) {
        Some(body) => {
            let content_type = mime(target.expect("body implies a resolved path").as_path());
            write_response(stream, 200, "OK", content_type, &body, keep_alive).await
        }
        None => {
            write_response(
                stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"Not found",
                keep_alive,
            )
            .await
        }
    }
}

/// Restrict requests to real files inside the UI root; no traversal, no percent-accepted paths.
fn sanitize(ui: &Path, path: &str) -> Option<PathBuf> {
    if path.contains('%') || path.contains('\\') {
        return None;
    }
    let relative = if path.is_empty() { "index.html" } else { path };
    if relative != "index.html"
        && relative
            .split('/')
            .any(|segment| segment.is_empty() || segment == ".")
    {
        return None;
    }
    let root = fs::canonicalize(ui).ok()?;
    let real = fs::canonicalize(ui.join(relative)).ok()?;
    (real.starts_with(&root)).then_some(real)
}

async fn write_response<S>(
    stream: &mut S,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    keep_alive: bool,
) -> Result<(), String>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let keep = if keep_alive { "keep-alive" } else { "close" };
    let mut frame = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nConnection: {keep}\r\n\r\n",
        body.len(),
    )
    .into_bytes();
    frame.extend_from_slice(body);
    stream.write_all(&frame).await.map_err(error)?;
    stream.flush().await.map_err(error)
}

fn origin_allowed(request: &Request) -> bool {
    let Some(origin) = request.headers.get("origin") else {
        return true;
    };
    // A browser always sends its own origin; a foreign page must not open the bridge (CSRF).
    let Some(authority) = request.headers.get("host") else {
        return false;
    };
    let origin = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or_default()
        .trim_end_matches('/');
    !origin.is_empty() && origin.eq_ignore_ascii_case(authority)
}

/// Complete the WebSocket handshake manually, then run the bridge session.
async fn websocket<S>(
    stream: &mut S,
    request: &Request,
    host: Arc<Host>,
    stopped: watch::Receiver<bool>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let handshake = if *stopped.borrow() {
        None
    } else if origin_allowed(request) {
        request
            .headers
            .get("sec-websocket-key")
            .filter(|key| (16..=64).contains(&key.len()))
            .cloned()
    } else {
        None
    };
    let Some(key) = handshake else {
        return write_response(
            stream,
            403,
            "Forbidden",
            "text/plain; charset=utf-8",
            b"Forbidden",
            false,
        )
        .await;
    };
    let accept = derive_accept_key(key.as_bytes());
    stream
        .write_all(
            format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(error)?;
    stream.flush().await.map_err(error)?;
    let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    session(&mut socket, host, stopped).await
}

// A web session accepts only the command names below; everything else fails with an
// explicit denial because project registration and host management stay desktop-only.
async fn dispatch(host: &Arc<Host>, command: &str, params: Value) -> Result<Value, String> {
    let string = |key: &str| {
        params[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("Missing {key}"))
    };
    match command {
        "server_connect" => host.request(true, "host/connect", json!({})).await,
        "server_request" => {
            let method = string("method")?;
            host.request(true, &method, params["params"].clone()).await
        }
        "server_respond" => {
            host.request(
                true,
                "approval/respond",
                json!(
                    {"id":params["id"],"result":params.get("result").cloned().unwrap_or(Value::Null),"error":params.get("error").cloned().unwrap_or(Value::Null)}
                ),
            )
            .await
        }
        "projects_load" => host.request(true, "projects/list", json!({})).await,
        "files_list" => host
            .request(
                true,
                "files/list",
                json!({"root":string("root")?,"path":params["path"].as_str().unwrap_or_default()}),
            )
            .await,
        "file_read" => host
            .request(true, "files/read", json!({"root":string("root")?,"path":string("path")?}))
            .await,
        "file_save" => host
            .request(
                true,
                "files/save",
                json!(
                    {"root":string("root")?,"path":string("path")?,"content":string("content")?,"expected":string("expected")?}
                ),
            )
            .await,
        _ => Err("This feature is only available in the desktop app".into()),
    }
}

async fn session<S>(
    socket: &mut WebSocketStream<S>,
    host: Arc<Host>,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (requests, mut incoming) = mpsc::channel::<Value>(16);
    let (responses, mut outgoing) = mpsc::channel::<Value>(16);
    let worker_host = host.clone();
    let worker = tokio::spawn(async move {
        while let Some(request) = incoming.recv().await {
            let id = request["id"].clone();
            let command = request["command"].as_str().unwrap_or_default().to_owned();
            let result = tokio::time::timeout(
                TIMEOUT,
                dispatch(&worker_host, &command, request["params"].clone()),
            )
            .await
            .unwrap_or_else(|_| Err("Request timed out".into()));
            let response = match result {
                Ok(result) => json!({"id":id,"result":result}),
                Err(message) => json!({"id":id,"error":message}),
            };
            if responses.send(response).await.is_err() {
                break;
            }
        }
    });
    struct Worker(JoinHandle<()>);
    impl Drop for Worker {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _worker = Worker(worker);
    let mut events = host.events.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut last_seen = Instant::now();
    loop {
        let message = tokio::select! {
            _ = stopped.changed() => break,
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) { break; }
                Some(Message::Ping(Vec::new().into()))
            }
            response = outgoing.recv() => response.map(|value| Message::Text(value.to_string().into())),
            event = events.recv() => {
                match event {
                    Ok(envelope) => host.remote_event(&envelope).map(|event| {
                        Message::Text(
                            json!({"method":"host-event","params":{"hostId":"local","event":event["event"],"epoch":event["epoch"],"sequence":event["sequence"]}})
                                .to_string()
                                .into(),
                        )
                    }),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Some(Message::Text(
                        json!({"method":"host-event","params":{"hostId":"local","event":{"method":"recodex/disconnected","params":{}}}})
                            .to_string()
                            .into(),
                    )),
                    Err(_) => break,
                }
            }
            message = socket.next() => {
                let Some(message) = message else { break };
                last_seen = Instant::now();
                match message.map_err(error)? {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text).map_err(error)?;
                        if requests.try_send(request.clone()).is_err() {
                            Some(Message::Text(
                                json!({"id":request["id"],"error":"Web request queue is full"})
                                    .to_string()
                                    .into(),
                            ))
                        } else {
                            None
                        }
                    }
                    Message::Ping(bytes) => Some(Message::Pong(bytes)),
                    Message::Close(_) => break,
                    _ => None,
                }
            }
        };
        if let Some(message) = message {
            tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.send(message))
                .await
                .map_err(|_| "Slow connection".to_string())?
                .map_err(error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{project_id, Fixture};
    use tokio_rustls::{
        client::TlsStream,
        rustls::{
            self,
            pki_types::{CertificateDer, ServerName},
            RootCertStore,
        },
        TlsConnector,
    };
    use tokio_tungstenite::{client_async_with_config, tungstenite::client::IntoClientRequest};

    fn certificate(directory: &Path) -> Vec<u8> {
        let identity: Value = serde_json::from_slice(
            &fs::read(directory.join("lan-identity.json")).expect("identity file"),
        )
        .expect("identity JSON");
        identity["certificate"]
            .as_array()
            .expect("certificate array")
            .iter()
            .map(|byte| byte.as_u64().expect("certificate byte") as u8)
            .collect()
    }

    async fn tls_socket(host_directory: &Path, address: &str) -> TlsStream<tokio::net::TcpStream> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate(host_directory)))
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let stream = TcpStream::connect(address).await.unwrap();
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("recodex.local").unwrap(), stream)
            .await
            .unwrap()
    }

    async fn http_get(fixture: &Fixture, address: &str, path: &str) -> String {
        let mut tls = tls_socket(&fixture.host.directory, address).await;
        tls.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    type BitStream = WebSocketStream<TlsStream<tokio::net::TcpStream>>;

    async fn open_api(fixture: &Fixture, address: &str, origin: Option<&str>) -> BitStream {
        let tls = tls_socket(&fixture.host.directory, address).await;
        let request = format!("wss://{address}/api")
            .into_client_request()
            .unwrap();
        let request = match origin {
            Some(origin) => {
                let (mut parts, body) = request.into_parts();
                parts.headers.insert("Origin", origin.parse().unwrap());
                tokio_tungstenite::tungstenite::http::Request::from_parts(parts, body)
            }
            None => request,
        };
        let (socket, _) = client_async_with_config(request, tls, None).await.unwrap();
        socket
    }

    async fn exchange(socket: &mut BitStream, value: Value) -> Value {
        socket
            .send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
        loop {
            let message = socket.next().await.unwrap().unwrap();
            if message.is_text() {
                return serde_json::from_str(message.to_text().unwrap()).unwrap();
            }
        }
    }

    async fn ui_fixture() -> (tempfile::TempDir, PathBuf) {
        let ui = tempfile::tempdir().unwrap();
        fs::write(ui.path().join("index.html"), "<html>ReCodex</html>").unwrap();
        fs::create_dir_all(ui.path().join("_app")).unwrap();
        fs::write(ui.path().join("_app/app.js"), "console.log(1)").unwrap();
        let path = ui.path().to_path_buf();
        (ui, path)
    }

    #[tokio::test]
    async fn static_files_stay_inside_the_ui_root() {
        let fixture = Fixture::new().await;
        let (ui, ui_path) = ui_fixture().await;
        let server = serve(fixture.host.clone(), "127.0.0.1:0", Some(ui_path))
            .await
            .unwrap_or_else(|e| panic!("serve failed: {e}"));
        let address = server.address.clone();
        let index = http_get(&fixture, &address, "/").await;
        assert!(index.starts_with("HTTP/1.1 200") && index.contains("ReCodex</html>"));
        assert!(index.contains("text/html"));
        let asset = http_get(&fixture, &address, "/_app/app.js").await;
        assert!(asset.contains("200") && asset.contains("text/javascript"));
        let info = http_get(&fixture, &address, "/api/info").await;
        assert!(info.contains(r#"{"web":true}"#));
        // Traversal and percent-encoded paths never resolve outside or inside the UI root.
        for path in [
            "/../lan-identity.json",
            "/%2e%2e/lan-identity.json",
            "/_app/../app.js",
        ] {
            assert!(
                http_get(&fixture, &address, path)
                    .await
                    .starts_with("HTTP/1.1 404"),
                "path traversal must fail: {path}"
            );
        }
        server.shutdown().await;
        drop(ui);
        fixture.host.shutdown().await;
    }

    #[tokio::test]
    async fn web_sessions_use_the_shared_project_boundary_and_denied_commands_fail() {
        let fixture = Fixture::new().await;
        let (ui, ui_path) = ui_fixture().await;
        let server = serve(fixture.host.clone(), "127.0.0.1:0", Some(ui_path))
            .await
            .unwrap();
        let address = server.address.clone();
        let mut socket = open_api(&fixture, &address, None).await;
        let projects = exchange(&mut socket, json!({"id":1,"command":"projects_load"})).await;
        assert_eq!(projects["result"].as_array().unwrap().len(), 1);
        assert_eq!(projects["result"][0]["name"], "Public");
        assert_eq!(projects["result"][0]["path"], project_id(&fixture.root));
        let file = exchange(
            &mut socket,
            json!({"id":2,"command":"server_request","params":{"method":"files/read","params":{"root":project_id(&fixture.root),"path":"main.txt"}}}),
        )
        .await;
        assert_eq!(file["result"], "original");
        // The private project is not shared, so its opaque root resolves against the shared list only.
        let private = exchange(
            &mut socket,
            json!({"id":3,"command":"server_request","params":{"method":"files/read","params":{"root":project_id(&fixture.private),"path":"secret.txt"}}}),
        )
        .await;
        assert!(private["error"].is_string());
        let denied = exchange(&mut socket, json!({"id":4,"command":"project_save"})).await;
        assert_eq!(
            denied["error"],
            "This feature is only available in the desktop app"
        );
        socket.close(None).await.unwrap();
        drop(ui);
        server.shutdown().await;
        fixture.host.shutdown().await;
    }

    #[tokio::test]
    async fn foreign_websocket_origins_are_rejected() {
        let fixture = Fixture::new().await;
        let (ui, ui_path) = ui_fixture().await;
        let server = serve(fixture.host.clone(), "127.0.0.1:0", Some(ui_path))
            .await
            .unwrap();
        let address = server.address.clone();
        let tls = tls_socket(&fixture.host.directory, &address).await;
        let request = format!("wss://{address}/api")
            .into_client_request()
            .unwrap();
        let (mut parts, body) = request.into_parts();
        parts
            .headers
            .insert("Origin", "https://evil.example".parse().unwrap());
        let request = tokio_tungstenite::tungstenite::http::Request::from_parts(parts, body);
        assert!(client_async_with_config(request, tls, None).await.is_err());
        // The matching origin upgrades and exchanges requests normally.
        let mut socket = open_api(&fixture, &address, Some(&format!("https://{address}"))).await;
        let projects = exchange(&mut socket, json!({"id":1,"command":"projects_load"})).await;
        assert!(projects["result"].is_array());
        socket.close(None).await.unwrap();
        drop(ui);
        server.shutdown().await;
        fixture.host.shutdown().await;
    }
}
