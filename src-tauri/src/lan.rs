//! Native TLS WebSocket transport. Credentials never go in URLs, localStorage, or logs.
use crate::{
    host::Host,
    lan_store::{LanStore, RemoteProfile, SavedDevice},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch, Mutex},
    task::JoinHandle,
};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
    },
    TlsAcceptor, TlsConnector,
};
use tokio_tungstenite::{
    tungstenite::{
        handshake::server::{ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
        Message,
    },
    WebSocketStream,
};
use uuid::Uuid;

const PROTOCOL: u64 = 1;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(65);
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}
fn text(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    certificate: Vec<u8>,
    key: Vec<u8>,
}
impl Identity {
    fn load(directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(error)?;
        let path = directory.join("lan-identity.json");
        if path.exists() {
            return serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error);
        }
        let key =
            rcgen::generate_simple_self_signed(vec!["recodex.local".into()]).map_err(error)?;
        let identity = Self {
            id: Uuid::new_v4().to_string(),
            certificate: key.cert.der().to_vec(),
            key: key.signing_key.serialize_der(),
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(error)?;
        file.write_all(&serde_json::to_vec(&identity).map_err(error)?)
            .map_err(error)?;
        file.sync_all().map_err(error)?;
        Ok(identity)
    }
    fn tls(&self) -> Result<TlsAcceptor, String> {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(error)?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(self.certificate.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone())),
        )
        .map_err(error)?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

pub fn local_id(directory: &Path) -> Result<String, String> {
    Ok(Identity::load(directory)?.id)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    version: u64,
    host_id: String,
    pub(crate) name: String,
    certificate: Vec<u8>,
    token: String,
}
impl Invitation {
    pub fn host_id(&self) -> &str {
        &self.host_id
    }
}
struct Pairing {
    digest: [u8; 32],
    expires: Instant,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
}
struct Credential {
    device: Device,
    digest: [u8; 32],
    revoked: watch::Sender<bool>,
}

pub struct Listener {
    pub id: String,
    pub name: String,
    pub address: String,
    certificate: Vec<u8>,
    store: Arc<LanStore>,
    pairing: StdMutex<Option<Pairing>>,
    devices: StdMutex<HashMap<String, Credential>>,
    stop: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl Listener {
    pub fn status(&self) -> Value {
        json!({"id":self.id,"name":self.name,"address":self.address,"devices":self.devices.lock().unwrap().values().map(|c| &c.device).collect::<Vec<_>>()})
    }
    pub fn invite(&self) -> Invitation {
        let token = secret();
        *self.pairing.lock().unwrap() = Some(Pairing {
            digest: Sha256::digest(token.as_bytes()).into(),
            expires: Instant::now() + Duration::from_secs(600),
        });
        Invitation {
            version: PROTOCOL,
            host_id: self.id.clone(),
            name: self.name.clone(),
            certificate: self.certificate.clone(),
            token,
        }
    }
    pub fn revoke(&self, id: &str) -> Result<(), String> {
        let mut devices = self.devices.lock().unwrap();
        self.store.update(|s| s.devices.retain(|d| d.id != id))?;
        devices.remove(id);
        Ok(())
    }
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    fn authenticate(
        &self,
        auth: &Value,
    ) -> Result<(Device, String, watch::Receiver<bool>), String> {
        if auth["version"] != PROTOCOL {
            return Err("Incompatible host protocol version".into());
        }
        let token = auth["token"]
            .as_str()
            .filter(|s| s.len() == 64)
            .ok_or("Invalid credential")?;
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if let Some(device) = self
            .devices
            .lock()
            .unwrap()
            .values()
            .find(|c| bool::from(c.digest.ct_eq(&digest)))
            .map(|c| (c.device.clone(), c.revoked.subscribe()))
        {
            return Ok((device.0, token.into(), device.1));
        }
        let mut invitation = self.pairing.lock().unwrap();
        if !invitation
            .as_ref()
            .is_some_and(|p| p.expires > Instant::now() && bool::from(p.digest.ct_eq(&digest)))
        {
            return Err(
                "Invalid or expired connection invitation. Create a new invitation on the host PC."
                    .into(),
            );
        }
        let device = Device {
            id: Uuid::new_v4().to_string(),
            name: auth["name"]
                .as_str()
                .unwrap_or("ReCodex PC")
                .chars()
                .take(80)
                .collect(),
        };
        let token = secret();
        let verifier = Sha256::digest(token.as_bytes()).into();
        self.store.update(|s| {
            s.devices.push(SavedDevice {
                id: device.id.clone(),
                name: device.name.clone(),
                verifier,
            })
        })?;
        *invitation = None;
        let (revoked, receiver) = watch::channel(false);
        self.devices.lock().unwrap().insert(
            device.id.clone(),
            Credential {
                device: device.clone(),
                digest: Sha256::digest(token.as_bytes()).into(),
                revoked,
            },
        );
        Ok((device, token, receiver))
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

pub async fn listen(
    host: Arc<Host>,
    address: &str,
    name: String,
    shared: Vec<String>,
) -> Result<Arc<Listener>, String> {
    if name.trim().is_empty() || name.len() > 80 {
        return Err("Choose a host name of 1–80 characters".into());
    }
    // Only locally registered roots can be published; sharing is always explicit.
    if shared.is_empty()
        || shared.iter().any(|root| {
            !host
                .workspace
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|p| &p.path == root)
        })
    {
        return Err("Select at least one registered project to share".into());
    }
    let identity = Identity::load(&host.directory)?;
    let tls = identity.tls()?;
    let socket = TcpListener::bind(address).await.map_err(error)?;
    let address = socket.local_addr().map_err(error)?.to_string();
    let (stop, mut stopped) = watch::channel(false);
    let listener = Arc::new(Listener {
        id: identity.id,
        name,
        address,
        certificate: identity.certificate,
        store: host.lan.clone(),
        pairing: StdMutex::new(None),
        devices: StdMutex::new(
            host.lan
                .snapshot()
                .devices
                .into_iter()
                .map(|saved| {
                    let (revoked, _) = watch::channel(false);
                    (
                        saved.id.clone(),
                        Credential {
                            device: Device {
                                id: saved.id,
                                name: saved.name,
                            },
                            digest: saved.verifier,
                            revoked,
                        },
                    )
                })
                .collect(),
        ),
        stop,
        task: StdMutex::new(None),
    });
    *host.shared.lock().unwrap() = shared;
    let weak = Arc::downgrade(&listener);
    let task = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        let permits = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                Some(_) = peers.join_next(), if !peers.is_empty() => {},
                result = socket.accept() => {
                    let Ok((stream, _)) = result else { break; };
                    let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                    let Some(listener) = weak.upgrade() else { break; };
                    let host = host.clone(); let tls = tls.clone(); let stopped = stopped.clone();
                    peers.spawn(async move {
                        let _permit = permit;
                        let _ = serve(stream, tls, host, listener, stopped).await;
                    });
                }
            }
        }
        peers.abort_all();
        while peers.join_next().await.is_some() {}
    });
    *listener.task.lock().unwrap() = Some(task);
    Ok(listener)
}

// Deny browser-origin connections even before application authentication.
#[allow(clippy::result_large_err)]
fn check_origin(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.headers().contains_key("origin") {
        let mut rejected = ErrorResponse::new(Some("Browser connections are not supported".into()));
        *rejected.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
        Err(rejected)
    } else {
        Ok(response)
    }
}
struct Session {
    worker: JoinHandle<()>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn receive<S>(socket: &mut WebSocketStream<S>) -> Result<Value, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .map_err(|_| "Host handshake timed out")?
        .ok_or("Connection closed")?
        .map_err(error)?;
    serde_json::from_str(message.to_text().map_err(error)?).map_err(error)
}

async fn serve(
    stream: TcpStream,
    tls: TlsAcceptor,
    host: Arc<Host>,
    listener: Arc<Listener>,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut socket = tokio::time::timeout(Duration::from_secs(10), async {
        let tls = tls.accept(stream).await.map_err(error)?;
        tokio_tungstenite::accept_hdr_async_with_config(tls, check_origin, Some(ws_config()))
            .await
            .map_err(error)
    })
    .await
    .map_err(|_| "TLS handshake timed out")??;
    let auth = receive(&mut socket).await?;
    let (device, token, mut revoked) = match listener.authenticate(&auth) {
        Ok(auth) => auth,
        Err(message) => {
            socket
                .send(text(json!({"error":message})))
                .await
                .map_err(error)?;
            return Ok(());
        }
    };
    socket.send(text(json!({"version":PROTOCOL,"hostId":listener.id,"name":listener.name,"deviceId":device.id,"token":token}))).await.map_err(error)?;
    let (requests, mut incoming) = mpsc::channel::<Value>(16);
    let (responses, mut outgoing) = mpsc::channel::<Value>(16);
    let worker_host = host.clone();
    let mut worker_revoked = revoked.clone();
    let worker = tokio::spawn(async move {
        while let Some(request) = incoming.recv().await {
            let id = request["id"].clone();
            let result = tokio::select! {
                biased;
                _ = worker_revoked.changed() => break,
                result = worker_host.request(true,request["method"].as_str().unwrap_or_default(),request["params"].clone()) => result,
            };
            let response = match result {
                Ok(result) => json!({"id":id,"result":result}),
                Err(message) => json!({"id":id,"error":message}),
            };
            if responses.send(response).await.is_err() {
                break;
            }
        }
    });
    let _session = Session { worker };
    let mut events = host.events.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut last_seen = Instant::now();
    loop {
        let message = tokio::select! {
            _ = stopped.changed() => break,
            _ = revoked.changed() => break,
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) || !listener.devices.lock().unwrap().contains_key(&device.id) { break; }
                Some(Message::Ping(Vec::new().into()))
            }
            response = outgoing.recv() => response.map(text),
            event = events.recv() => {
                let event = event.map_err(|_| "Event stream overflowed; reconnect to reload host state")?;
                host.remote_event(&event).map(|e| text(json!({"method":"host/event","params":e})))
            }
            message = socket.next() => {
                let Some(message) = message else { break; };
                last_seen = Instant::now();
                match message.map_err(error)? {
                    Message::Text(message) => {
                        let request: Value = serde_json::from_str(&message).map_err(error)?;
                        if requests.try_send(request.clone()).is_err() { Some(text(json!({"id":request["id"],"error":"Host request queue is full"}))) } else { None }
                    }
                    Message::Ping(bytes) => Some(Message::Pong(bytes)),
                    Message::Close(_) => break,
                    _ => None,
                }
            }
        };
        if let Some(message) = message {
            tokio::time::timeout(Duration::from_secs(10), socket.send(message))
                .await
                .map_err(|_| "Slow connection")?
                .map_err(error)?;
        }
    }
    Ok(())
}

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
struct PendingRequest {
    id: u64,
    pending: Pending,
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
pub struct Remote {
    pub id: String,
    pub name: String,
    endpoints: StdMutex<Vec<String>>,
    invitation: StdMutex<Invitation>,
    pending: Pending,
    outgoing: Mutex<Option<mpsc::Sender<Value>>>,
    task: StdMutex<Option<JoinHandle<()>>>,
    next: std::sync::atomic::AtomicU64,
}
impl Remote {
    pub fn new(endpoint: String, invitation: Invitation) -> Arc<Self> {
        Arc::new(Self {
            id: invitation.host_id.clone(),
            name: invitation.name.clone(),
            endpoints: StdMutex::new(vec![endpoint]),
            invitation: StdMutex::new(invitation),
            pending: Arc::default(),
            outgoing: Mutex::new(None),
            task: StdMutex::new(None),
            next: std::sync::atomic::AtomicU64::new(1),
        })
    }
    pub fn restore(profile: RemoteProfile) -> Arc<Self> {
        let remote = Self::new(
            String::new(),
            Invitation {
                version: profile.version,
                host_id: profile.id,
                name: profile.name,
                certificate: profile.certificate,
                token: profile.token,
            },
        );
        *remote.endpoints.lock().unwrap() = profile.endpoints;
        remote
    }
    pub fn update_endpoints(&self, endpoints: Vec<String>) {
        if !endpoints.is_empty() {
            *self.endpoints.lock().unwrap() = endpoints;
        }
    }
    pub fn profile(&self) -> RemoteProfile {
        let invitation = self.invitation.lock().unwrap();
        RemoteProfile {
            id: self.id.clone(),
            name: self.name.clone(),
            version: invitation.version,
            certificate: invitation.certificate.clone(),
            token: invitation.token.clone(),
            endpoints: self.endpoints.lock().unwrap().clone(),
        }
    }
    pub async fn connect(
        self: &Arc<Self>,
        emit: impl Fn(Value) + Send + 'static,
    ) -> Result<(), String> {
        let mut outgoing = self.outgoing.lock().await;
        if outgoing.as_ref().is_some_and(|s| !s.is_closed()) {
            return Ok(());
        }
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        let invitation = self.invitation.lock().unwrap().clone();
        if invitation.version != PROTOCOL {
            return Err("Incompatible host protocol version".into());
        }
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(invitation.certificate))
            .map_err(error)?;
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(error)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let endpoints = self.endpoints.lock().unwrap().clone();
        let mut connected = None;
        let mut failure = "Host not found on the LAN yet".to_string();
        for endpoint in endpoints.iter().take(8) {
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                let stream = TcpStream::connect(endpoint).await.map_err(error)?;
                let tls = TlsConnector::from(Arc::new(config.clone()))
                    .connect(ServerName::try_from("recodex.local").unwrap(), stream)
                    .await
                    .map_err(error)?;
                let (socket, _) = tokio_tungstenite::client_async_with_config(
                    format!("wss://{endpoint}/"),
                    tls,
                    Some(ws_config()),
                )
                .await
                .map_err(error)?;
                Ok::<_, String>(socket)
            })
            .await
            .unwrap_or_else(|_| Err("Host connection timed out".into()));
            match result {
                Ok(socket) => {
                    connected = Some(socket);
                    break;
                }
                Err(e) => failure = e,
            }
        }
        let mut socket = connected.ok_or(failure)?;
        socket.send(text(json!({"version":PROTOCOL,"token":invitation.token,"name":std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or("ReCodex PC".into())}))).await.map_err(error)?;
        let response = receive(&mut socket).await?;
        if let Some(message) = response["error"].as_str() {
            return Err(message.into());
        }
        if response["version"] != PROTOCOL || response["hostId"] != self.id {
            return Err("Host identity or protocol mismatch".into());
        }
        self.invitation.lock().unwrap().token = response["token"]
            .as_str()
            .ok_or("Missing device credential")?
            .into();
        let (sender, mut incoming) = mpsc::channel(16);
        *outgoing = Some(sender);
        let pending = self.pending.clone();
        let task = tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
            let mut last_seen = Instant::now();
            loop {
                let message = tokio::select! {
                    _ = heartbeat.tick() => {
                        if last_seen.elapsed() > Duration::from_secs(35) { break; }
                        Some(Message::Ping(Vec::new().into()))
                    }
                    request = incoming.recv() => { let Some(request) = request else { break; }; Some(text(request)) }
                    message = socket.next() => {
                        let Some(Ok(message)) = message else { break; };
                        last_seen = Instant::now();
                        match message {
                            Message::Text(message) => {
                                let Ok(value) = serde_json::from_str::<Value>(&message) else { break; };
                                if value["method"] == "host/event" { emit(value["params"].clone()); }
                                else if let Some(id) = value["id"].as_u64() {
                                    if let Some(reply) = pending.lock().unwrap().remove(&id) {
                                        let result = value["error"].as_str().map(|e| Err(e.into())).unwrap_or_else(|| Ok(value["result"].clone()));
                                        let _ = reply.send(result);
                                    }
                                }
                                None
                            }
                            Message::Ping(bytes) => Some(Message::Pong(bytes)),
                            Message::Close(_) => break,
                            _ => None,
                        }
                    }
                };
                if let Some(message) = message {
                    if !matches!(
                        tokio::time::timeout(Duration::from_secs(10), socket.send(message)).await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                }
            }
            for (_, reply) in pending.lock().unwrap().drain() {
                let _ = reply.send(Err(
                    "Connection lost. Reconnect and check the chat before retrying a change."
                        .into(),
                ));
            }
            emit(json!({"event":{"method":"recodex/disconnected","params":{}}}));
        });
        *self.task.lock().unwrap() = Some(task);
        Ok(())
    }
    pub async fn disconnect(&self) {
        *self.outgoing.lock().await = None;
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        for (_, reply) in self.pending.lock().unwrap().drain() {
            let _ = reply.send(Err("Host disconnected".into()));
        }
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let sender = self
            .outgoing
            .lock()
            .await
            .clone()
            .ok_or("Host is not connected")?;
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, reply);
        let _pending = PendingRequest {
            id,
            pending: self.pending.clone(),
        };
        let result = tokio::time::timeout(TIMEOUT, async {
            sender
                .send(json!({"id":id,"method":method,"params":params}))
                .await
                .map_err(|_| "Host disconnected".to_string())?;
            receiver
                .await
                .map_err(|_| "Host disconnected".to_string())?
        })
        .await
        .map_err(|_| {
            "Request timed out. Reconnect and check the chat before retrying a change.".to_string()
        })
        .and_then(|r| r);
        self.pending.lock().unwrap().remove(&id);
        result
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{project_id, Fixture};

    #[tokio::test]
    async fn tls_pairing_files_chat_approval_and_reconnection() {
        let fixture = Fixture::new().await;
        let listener = listen(
            fixture.host.clone(),
            "127.0.0.1:0",
            "Test PC".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let invitation = listener.invite();
        let remote = Remote::new(listener.address.clone(), invitation.clone());
        let (events, mut received) = mpsc::unbounded_channel();
        remote
            .connect(move |e| {
                let _ = events.send(e);
            })
            .await
            .unwrap();
        // Invitations are one-use; another connection needs its own invitation.
        assert!(Remote::new(listener.address.clone(), invitation)
            .connect(|_| {})
            .await
            .is_err());
        let projects = remote.request("projects/list", json!({})).await.unwrap();
        assert_eq!(projects.as_array().unwrap().len(), 1);
        assert_eq!(
            remote
                .request(
                    "files/read",
                    json!({"root":project_id(&fixture.root),"path":"main.txt"})
                )
                .await
                .unwrap(),
            "original"
        );
        remote.request("files/save",json!({"root":project_id(&fixture.root),"path":"main.txt","content":"remote","expected":"original"})).await.unwrap();
        fixture.host.request(false,"files/save",json!({"root":fixture.root,"path":"main.txt","content":"local","expected":"remote"})).await.unwrap();
        remote
            .request("thread/start", json!({"cwd":project_id(&fixture.root)}))
            .await
            .unwrap();
        remote
            .request(
                "turn/start",
                json!({"threadId":"shared-thread","input":[{"type":"text","text":"test"}]}),
            )
            .await
            .unwrap();
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = received.recv().await.unwrap();
                if event["event"].get("id").is_some() {
                    break event["event"]["id"].clone();
                }
            }
        })
        .await
        .unwrap();
        let peer = Remote::new(listener.address.clone(), listener.invite());
        peer.connect(|_| {}).await.unwrap();
        let pending = peer.request("host/snapshot", json!({})).await.unwrap();
        assert_eq!(pending["approvals"][0]["id"], approval);
        peer.request(
            "approval/respond",
            json!({"id":approval,"result":{"decision":"accept"}}),
        )
        .await
        .unwrap();
        assert!(remote
            .request(
                "approval/respond",
                json!({"id":approval,"result":{"decision":"decline"}})
            )
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = received.recv().await.unwrap();
                if event["event"]["method"] == "serverRequest/resolved" {
                    assert_eq!(event["event"]["params"]["requestId"], approval);
                    break;
                }
            }
        })
        .await
        .unwrap();
        peer.request("files/save",json!({"root":project_id(&fixture.root),"path":"main.txt","content":"peer edit","expected":"local"})).await.unwrap();
        assert_eq!(
            remote
                .request(
                    "files/read",
                    json!({"root":project_id(&fixture.root),"path":"main.txt"})
                )
                .await
                .unwrap(),
            "peer edit"
        );
        peer.disconnect().await;
        // Simulate a broken socket, retaining the device credential for reconnect.
        remote.task.lock().unwrap().take().unwrap().abort();
        *remote.outgoing.lock().await = None;
        remote.connect(|_| {}).await.unwrap();
        assert_eq!(
            remote.request("projects/list", json!({})).await.unwrap(),
            projects
        );
        for device in listener.status()["devices"].as_array().unwrap() {
            listener.revoke(device["id"].as_str().unwrap()).unwrap();
        }
        assert!(remote
            .request(
                "files/read",
                json!({"root":project_id(&fixture.root),"path":"main.txt"})
            )
            .await
            .is_err());
        listener.shutdown().await;
        fixture.host.shutdown().await;
    }
    #[tokio::test]
    async fn wrong_certificates_expired_invites_and_browser_origins_are_rejected() {
        let fixture = Fixture::new().await;
        let listener = listen(
            fixture.host.clone(),
            "127.0.0.1:0",
            "Test PC".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let mut invitation = listener.invite();
        let other = rcgen::generate_simple_self_signed(vec!["recodex.local".into()]).unwrap();
        invitation.certificate = other.cert.der().to_vec();
        assert!(Remote::new(listener.address.clone(), invitation)
            .connect(|_| {})
            .await
            .is_err());
        let expired = listener.invite();
        listener.pairing.lock().unwrap().as_mut().unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert!(Remote::new(listener.address.clone(), expired)
            .connect(|_| {})
            .await
            .is_err());
        let request = Request::builder()
            .header("Origin", "https://example.com")
            .body(())
            .unwrap();
        assert!(check_origin(&request, Response::new(())).is_err());
        listener.shutdown().await;
        fixture.host.shutdown().await;
    }
    #[tokio::test]
    async fn remembered_devices_reconnect_after_restarts_and_revocation_survives_restart() {
        let fixture = Fixture::new().await;
        let first = listen(
            fixture.host.clone(),
            "127.0.0.1:0",
            "PC".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let client = Remote::new(first.address.clone(), first.invite());
        client.connect(|_| {}).await.unwrap();
        let client_directory = tempfile::tempdir().unwrap();
        let client_store = LanStore::load(client_directory.path()).unwrap();
        client_store.remember(client.profile()).unwrap();
        client.disconnect().await;
        first.shutdown().await;
        // A new Host and client load their trust independently, as after both apps restart.
        let restarted = Host::new(fixture.host.directory.clone()).unwrap();
        let second = listen(
            restarted.clone(),
            "127.0.0.1:0",
            "PC".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let restored = Remote::restore(
            LanStore::load(client_directory.path())
                .unwrap()
                .snapshot()
                .remotes
                .remove(0),
        );
        restored.update_endpoints(vec!["127.0.0.1:0".into(), second.address.clone()]);
        restored.connect(|_| {}).await.unwrap();
        assert_eq!(
            restored
                .request(
                    "files/read",
                    json!({"root":project_id(&fixture.root),"path":"main.txt"})
                )
                .await
                .unwrap(),
            "original"
        );
        let id = second.status()["devices"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        second.revoke(&id).unwrap();
        restored.disconnect().await;
        second.shutdown().await;
        let third = listen(
            Host::new(fixture.host.directory.clone()).unwrap(),
            "127.0.0.1:0",
            "PC".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let revoked = Remote::restore(client.profile());
        revoked.update_endpoints(vec![third.address.clone()]);
        assert!(revoked.connect(|_| {}).await.is_err());
        third.shutdown().await;
        fixture.host.shutdown().await;
    }
}
