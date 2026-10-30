//! Transport-independent owner of a PC's Codex process and workspace.
use crate::{
    server::Server,
    workspace::{self, Workspace},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

const LEASE_TIME: Duration = Duration::from_secs(35);
pub const LOCAL: &str = "local";

struct Lease {
    owner: String,
    expires: Instant,
}
#[derive(Default)]
struct Runtime {
    approvals: HashMap<String, (Value, Value)>,
    active: HashMap<String, Value>,
    completed: VecDeque<String>,
    thread_roots: HashMap<String, String>,
    leases: HashMap<String, Lease>,
    sequence: u64,
    history: VecDeque<Value>,
    history_bytes: usize,
    epoch: String,
}

pub struct Host {
    pub lan: Arc<crate::lan_store::LanStore>,
    pub directory: PathBuf,
    pub workspace: Workspace,
    pub events: broadcast::Sender<Value>,
    server: Mutex<Option<Arc<Server>>>,
    // One gate covers checking ownership and accepting a mutation, including approvals.
    gate: Mutex<()>,
    runtime: StdMutex<Runtime>,
    pub shared: StdMutex<Vec<String>>,
}

pub fn project_id(path: &str) -> String {
    format!("{:x}", Sha256::digest(path.as_bytes()))[..24].to_owned()
}

impl Host {
    pub fn new(directory: PathBuf) -> Result<Arc<Self>, String> {
        let workspace = Workspace::default();
        workspace::projects_load(&directory, &workspace)?;
        let (events, _) = broadcast::channel(2048);
        Ok(Arc::new(Self {
            lan: crate::lan_store::LanStore::load(&directory)?,
            directory,
            workspace,
            events,
            server: Mutex::new(None),
            gate: Mutex::new(()),
            runtime: StdMutex::new(Runtime {
                epoch: Uuid::new_v4().to_string(),
                ..Runtime::default()
            }),
            shared: StdMutex::new(Vec::new()),
        }))
    }

    pub async fn connect(self: &Arc<Self>) -> Result<(), String> {
        let mut server = self.server.lock().await;
        if server.as_ref().is_some_and(|s| s.is_running()) {
            return Ok(());
        }
        *server = None;
        {
            let mut state = self.runtime.lock().unwrap();
            state.approvals.clear();
            state.active.clear();
            state.completed.clear();
            state.thread_roots.clear();
        }
        let weak = Arc::downgrade(self);
        let generation = Uuid::new_v4().to_string();
        {
            let mut state = self.runtime.lock().unwrap();
            state.epoch = generation.clone();
            state.sequence = 0;
            state.history.clear();
            state.history_bytes = 0;
        }
        *server = Some(Arc::new(
            Server::start(move |event| {
                if let Some(host) = weak.upgrade() {
                    host.accept_event(&generation, event);
                }
            })
            .await?,
        ));
        Ok(())
    }

    fn accept_event(self: &Arc<Self>, generation: &str, mut event: Value) {
        let mut state = self.runtime.lock().unwrap();
        if state.epoch != generation {
            return;
        }
        if let Some(id) = event.get("id").cloned() {
            let supported = matches!(
                event["method"].as_str().unwrap_or_default(),
                "item/commandExecution/requestApproval"
                    | "item/fileChange/requestApproval"
                    | "item/tool/requestUserInput"
                    | "item/permissions/requestApproval"
            );
            if !supported {
                let host = self.clone();
                let generation = generation.to_owned();
                tokio::spawn(async move {
                    let server = host.server.lock().await;
                    if host.runtime.lock().unwrap().epoch != generation {
                        return;
                    }
                    if let Some(server) = server.clone() {
                        let _ = server.write(json!({"id":id,"error":{"code":-32601,"message":"ReCodex does not support this server request"}})).await;
                    }
                });
                return;
            }
            let public_id = format!("{generation}:{id}");
            event["id"] = json!(public_id);
            state.approvals.insert(public_id, (id, event.clone()));
        }
        self.publish_locked(&mut state, event);
    }

    pub fn publish(&self, event: Value) {
        self.publish_locked(&mut self.runtime.lock().unwrap(), event);
    }
    fn publish_locked(&self, state: &mut Runtime, mut event: Value) {
        if event["method"] == "thread/started" {
            if let (Some(id), Some(cwd)) = (
                event["params"]["thread"]["id"].as_str(),
                event["params"]["thread"]["cwd"].as_str(),
            ) {
                state.thread_roots.insert(id.into(), cwd.into());
                event["params"]["threadId"] = json!(id);
            }
        }
        if let Some(root) = event["params"]["threadId"]
            .as_str()
            .and_then(|id| state.thread_roots.get(id))
        {
            event["params"]["projectId"] = json!(project_id(root));
        }
        let thread = event["params"]["threadId"].as_str().unwrap_or_default();
        match event["method"].as_str().unwrap_or_default() {
            "turn/started" => {
                state
                    .active
                    .insert(thread.into(), event["params"]["turn"].clone());
            }
            "turn/completed" => {
                state.active.remove(thread);
                if let Some(id) = event["params"]["turn"]["id"].as_str() {
                    state.completed.push_back(id.into());
                    if state.completed.len() > 4096 {
                        state.completed.pop_front();
                    }
                }
                state.approvals.retain(|_, (_, e)| {
                    e["params"]["threadId"] != thread
                        || e["params"]["turnId"] != event["params"]["turn"]["id"]
                });
            }
            "recodex/disconnected" => {
                state.active.clear();
                state.approvals.clear();
            }
            "serverRequest/resolved" => {
                let raw = event["params"]["requestId"].clone();
                if let Some(public) = state
                    .approvals
                    .iter()
                    .find(|(_, (id, _))| id == &raw)
                    .map(|(id, _)| id.clone())
                {
                    state.approvals.remove(&public);
                    event["params"]["requestId"] = json!(public);
                }
            }
            _ => {}
        }
        state.sequence += 1;
        let envelope = json!({"epoch": state.epoch, "sequence":state.sequence,"event":event});
        state.history_bytes += envelope.to_string().len();
        state.history.push_back(envelope.clone());
        while state.history_bytes > 4 * 1024 * 1024 || state.history.len() > 8192 {
            if let Some(old) = state.history.pop_front() {
                state.history_bytes -= old.to_string().len();
            }
        }
        let _ = self.events.send(envelope);
    }

    fn is_shared(&self, state: &Runtime, thread: &str) -> bool {
        state
            .thread_roots
            .get(thread)
            .is_some_and(|root| self.shared.lock().unwrap().contains(root))
    }
    fn remote_event_allowed(&self, state: &Runtime, envelope: &Value) -> bool {
        let event = &envelope["event"];
        let method = event["method"].as_str().unwrap_or_default();
        event["params"]["threadId"]
            .as_str()
            .is_some_and(|id| self.is_shared(state, id))
            || matches!(
                method,
                "recodex/disconnected"
                    | "account/updated"
                    | "account/rateLimits/updated"
                    | "serverRequest/resolved"
            )
    }
    pub fn remote_event(&self, envelope: &Value) -> Option<Value> {
        self.remote_event_allowed(&self.runtime.lock().unwrap(), envelope)
            .then(|| envelope.clone())
    }
    fn remember_threads(&self, result: &Value) {
        let mut state = self.runtime.lock().unwrap();
        let threads: Vec<&Value> = result["data"]
            .as_array()
            .map(|v| v.iter().collect())
            .unwrap_or_else(|| vec![&result["thread"]]);
        for thread in threads {
            if let (Some(id), Some(cwd)) = (thread["id"].as_str(), thread["cwd"].as_str()) {
                state.thread_roots.insert(id.into(), cwd.into());
            }
        }
    }
    pub fn snapshot(&self, remote: bool) -> Value {
        self.snapshot_locked(&self.runtime.lock().unwrap(), remote)
    }
    fn snapshot_locked(&self, state: &Runtime, remote: bool) -> Value {
        json!({"epoch":state.epoch,"sequence":state.sequence,
            "approvals":state.approvals.values().filter(|(_, e)| !remote || e["params"]["threadId"].as_str().is_some_and(|id| self.is_shared(state,id))).map(|(_, event)|event).collect::<Vec<_>>(),
            "activeTurns":state.active.iter().filter(|(id,_)| !remote || self.is_shared(state,id)).collect::<HashMap<_,_>>()})
    }

    pub fn keepalive(&self, owner: &str) {
        for lease in self
            .runtime
            .lock()
            .unwrap()
            .leases
            .values_mut()
            .filter(|l| l.owner == owner)
        {
            lease.expires = Instant::now() + LEASE_TIME;
        }
    }
    pub fn release(&self, owner: &str) {
        self.runtime
            .lock()
            .unwrap()
            .leases
            .retain(|_, l| l.owner != owner);
    }
    fn claim(&self, root: &str, owner: &str) -> Result<(), String> {
        let mut state = self.runtime.lock().unwrap();
        if state
            .leases
            .get(root)
            .is_some_and(|l| l.owner != owner && l.expires > Instant::now())
        {
            return Err("Another device controls this project. Use Take control in Connections to hand it over.".into());
        }
        state.leases.insert(
            root.into(),
            Lease {
                owner: owner.into(),
                expires: Instant::now() + LEASE_TIME,
            },
        );
        Ok(())
    }
    fn root(&self, key: &str, remote: bool) -> Result<String, String> {
        let projects = self.workspace.0.lock().unwrap();
        let project = projects
            .iter()
            .find(|p| {
                if remote {
                    project_id(&p.path) == key
                } else {
                    p.path == key || p.history_paths.iter().any(|p| p == key)
                }
            })
            .ok_or("Unknown project")?;
        if remote && !self.shared.lock().unwrap().contains(&project.path) {
            return Err("This project is not shared".into());
        }
        Ok(if remote {
            project.path.clone()
        } else {
            key.into()
        })
    }
    async fn codex(&self, method: &str, params: Value) -> Result<Value, String> {
        let server = self
            .server
            .lock()
            .await
            .clone()
            .ok_or("Codex is not connected")?;
        let result = server.request(method, params).await?;
        self.remember_threads(&result);
        Ok(result)
    }
    async fn thread_root(&self, id: &str, remote: bool) -> Result<String, String> {
        let thread = self
            .codex("thread/read", json!({"threadId":id,"includeTurns":false}))
            .await?;
        let cwd = thread["thread"]["cwd"]
            .as_str()
            .ok_or("Thread has no working directory")?;
        let key = if remote { project_id(cwd) } else { cwd.into() };
        self.root(&key, remote)
    }

    pub async fn request(
        self: &Arc<Self>,
        owner: &str,
        remote: bool,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let _gate = self.gate.lock().await;
        let string = |key: &str| {
            params[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Missing {key}"))
        };
        match method {
            "host/connect" => {
                self.connect().await?;
                Ok(Value::Null)
            }
            "host/snapshot" => Ok(self.snapshot(remote)),
            "host/replay" => {
                let state = self.runtime.lock().unwrap();
                let after = params["sequence"].as_u64().unwrap_or(0);
                let reset = params["epoch"] != state.epoch
                    || after > state.sequence
                    || state.history.front().is_some_and(|v| {
                        after.saturating_add(1) < v["sequence"].as_u64().unwrap_or(0)
                    });
                let events: Vec<_> = if reset {
                    Vec::new()
                } else {
                    state
                        .history
                        .iter()
                        .filter(|e| e["sequence"].as_u64().unwrap_or(0) > after)
                        .filter(|e| !remote || self.remote_event_allowed(&state, e))
                        .cloned()
                        .collect()
                };
                let epoch = state.epoch.clone();
                let sequence = state.sequence;
                let snapshot = self.snapshot_locked(&state, remote);
                Ok(
                    json!({"reset":reset,"epoch":epoch,"sequence":sequence,"events":events,"snapshot":snapshot}),
                )
            }
            "host/keepalive" => {
                self.keepalive(owner);
                Ok(Value::Null)
            }
            "host/release" => {
                self.release(owner);
                Ok(Value::Null)
            }
            "host/acquire" => {
                let root = self.root(&string("project")?, remote)?;
                self.runtime.lock().unwrap().leases.insert(
                    root,
                    Lease {
                        owner: owner.into(),
                        expires: Instant::now() + LEASE_TIME,
                    },
                );
                Ok(Value::Null)
            }
            "projects/list" => {
                let projects = self.workspace.0.lock().unwrap();
                let shared = self.shared.lock().unwrap();
                Ok(Value::Array(
                    projects
                        .iter()
                        .filter(|p| !remote || shared.contains(&p.path))
                        .map(|p| {
                            if remote {
                                json!({"path":project_id(&p.path),"name":p.name,"historyPaths":[]})
                            } else {
                                json!(p)
                            }
                        })
                        .collect(),
                ))
            }
            "files/list" | "files/read" | "files/save" => {
                let root = self.root(&string("root")?, remote)?;
                let path = string("path")?;
                match method {
                    "files/list" => {
                        serde_json::to_value(workspace::files_list(&self.workspace, root, path)?)
                            .map_err(|e| e.to_string())
                    }
                    "files/read" => Ok(json!(workspace::file_read(&self.workspace, root, path)?)),
                    _ => {
                        self.claim(&root, owner)?;
                        workspace::file_save(
                            &self.workspace,
                            root,
                            path,
                            string("content")?,
                            string("expected")?,
                        )?;
                        Ok(Value::Null)
                    }
                }
            }
            "model/list" => self.codex(method, pick(&params, &["cursor"])).await,
            "account/read" | "account/rateLimits/read" => self.codex(method, json!({})).await,
            "account/login/start" if !remote => self.codex(method, json!({"type":"chatgpt"})).await,
            "thread/list" => {
                let root = self.root(&string("cwd")?, remote)?;
                let mut input = pick(&params, &["cursor", "limit", "sortKey"]);
                input["cwd"] = json!(root);
                if let Some(limit) = input["limit"].as_u64() {
                    input["limit"] = json!(limit.min(100));
                }
                let mut result = self.codex(method, input).await?;
                if remote {
                    if let Some(threads) = result["data"].as_array_mut() {
                        threads.retain(|t| t["cwd"] == root);
                    }
                }
                Ok(result)
            }
            "thread/start" => {
                let root = self.root(&string("cwd")?, remote)?;
                self.claim(&root, owner)?;
                let mut input = pick(&params, &["model"]);
                input["cwd"] = json!(root);
                input["sandbox"] = json!("workspace-write");
                input["approvalPolicy"] = json!("on-request");
                self.codex(method, input).await
            }
            "thread/read" | "thread/resume" | "thread/delete" | "turn/start" | "turn/interrupt" => {
                let id = string("threadId")?;
                let root = self.thread_root(&id, remote).await?;
                if method != "thread/read" && method != "thread/resume" {
                    self.claim(&root, owner)?;
                }
                if method == "turn/start" {
                    let inputs = params["input"]
                        .as_array()
                        .ok_or("A text message is required")?;
                    if inputs.is_empty()
                        || inputs.len() > 32
                        || inputs.iter().any(|input| {
                            input["type"] != "text"
                                || input["text"].as_str().is_none_or(|s| s.len() > 1024 * 1024)
                        })
                    {
                        return Err("Only text messages up to 1 MB are supported".into());
                    }
                }
                if method == "turn/start" && self.runtime.lock().unwrap().active.contains_key(&id) {
                    return Err("This chat already has a running turn".into());
                }
                if method == "thread/delete"
                    && self.runtime.lock().unwrap().active.contains_key(&id)
                {
                    return Err("Stop the running turn before deleting this chat".into());
                }
                let mut input = match method {
                    "turn/start" => pick(
                        &params,
                        &["threadId", "input", "model", "effort", "summary"],
                    ),
                    "turn/interrupt" => pick(&params, &["threadId", "turnId"]),
                    _ => json!({"threadId":id}),
                };
                if method == "thread/read" {
                    input["includeTurns"] = json!(true);
                }
                if method == "thread/resume" {
                    // Resuming can change runtime configuration; observers only read an already loaded thread.
                    let observing = {
                        let state = self.runtime.lock().unwrap();
                        state.active.contains_key(&id)
                            || state
                                .leases
                                .get(&root)
                                .is_some_and(|l| l.owner != owner && l.expires > Instant::now())
                    };
                    if observing {
                        let mut read = self
                            .codex("thread/read", json!({"threadId":id,"includeTurns":true}))
                            .await?;
                        read["model"] = read["thread"]["model"].clone();
                        read["reasoningEffort"] = Value::Null;
                        return Ok(read);
                    }
                    input["sandbox"] = json!("workspace-write");
                    input["approvalPolicy"] = json!("on-request");
                }
                let result = self.codex(method, input).await?;
                if method == "turn/start" {
                    let mut state = self.runtime.lock().unwrap();
                    let turn = &result["turn"];
                    if turn["status"] == "inProgress"
                        && !state
                            .completed
                            .iter()
                            .any(|id| Some(id.as_str()) == turn["id"].as_str())
                    {
                        state.active.insert(id, turn.clone());
                    }
                }
                Ok(result)
            }
            "approval/respond" => {
                let public_id = string("id")?;
                let (raw_id, event) = self
                    .runtime
                    .lock()
                    .unwrap()
                    .approvals
                    .get(&public_id)
                    .cloned()
                    .ok_or(
                        "This request has already been resolved or belongs to an old Codex session",
                    )?;
                if let Some(thread) = event["params"]["threadId"].as_str() {
                    let root = self.thread_root(thread, remote).await?;
                    self.claim(&root, owner)?;
                } else if remote {
                    return Err("This request must be answered on the host PC".into());
                }
                let response = if params.get("error").is_some_and(|v| !v.is_null()) {
                    json!({"id":raw_id,"error":params["error"]})
                } else {
                    json!({"id":raw_id,"result":params["result"]})
                };
                let server = self
                    .server
                    .lock()
                    .await
                    .clone()
                    .ok_or("Codex is not connected")?;
                server.write(response).await?;
                self.runtime.lock().unwrap().approvals.remove(&public_id);
                self.publish(
                    json!({"method":"serverRequest/resolved","params":{"requestId":public_id}}),
                );
                Ok(Value::Null)
            }
            _ => Err(format!("Operation {method} is not available on this host")),
        }
    }

    pub async fn shutdown(&self) {
        *self.server.lock().await = None;
    }
}

fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut result = json!({});
    for key in keys {
        if let Some(v) = value.get(*key) {
            result[*key] = v.clone();
        }
    }
    result
}

#[cfg(test)]
pub(crate) struct Fixture {
    pub host: Arc<Host>,
    pub root: String,
    pub private: String,
    directory: PathBuf,
}
#[cfg(test)]
impl Fixture {
    pub async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("recodex-host-{}", Uuid::new_v4()));
        let root = directory.join("public");
        let private = directory.join("private");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(root.join("main.txt"), "original").unwrap();
        std::fs::write(private.join("secret.txt"), "secret").unwrap();
        let host = Host::new(directory.join("data")).unwrap();
        let root = std::fs::canonicalize(root)
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .to_owned();
        let private = std::fs::canonicalize(private)
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .to_owned();
        workspace::project_save(
            &host.directory,
            &host.workspace,
            root.clone(),
            Some("Public".into()),
        )
        .unwrap();
        workspace::project_save(
            &host.directory,
            &host.workspace,
            private.clone(),
            Some("Private".into()),
        )
        .unwrap();
        *host.shared.lock().unwrap() = vec![root.clone()];
        let mut command = tokio::process::Command::new("node");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/codex.mjs"
            ))
            .env("RECODEX_TEST_PROJECT", &root)
            .env("RECODEX_TEST_PRIVATE", &private);
        let weak = Arc::downgrade(&host);
        let epoch = host.runtime.lock().unwrap().epoch.clone();
        *host.server.lock().await = Some(Arc::new(
            Server::start_command(command, move |event| {
                if let Some(host) = weak.upgrade() {
                    host.accept_event(&epoch, event);
                }
            })
            .await
            .unwrap(),
        ));
        Self {
            host,
            root,
            private,
            directory,
        }
    }
}
#[cfg(test)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn published_roots_paths_and_method_policy_are_enforced() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let list = host
            .request("device", true, "projects/list", json!({}))
            .await
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["path"], project_id(&fixture.root));
        assert!(!list.to_string().contains(&fixture.root));
        for method in [
            "command/exec",
            "account/login/start",
            "config/value/write",
            "process/spawn",
        ] {
            assert!(host
                .request("device", true, method, json!({}))
                .await
                .is_err());
        }
        for root in [
            project_id(&fixture.private),
            fixture.root.clone(),
            "unknown".into(),
        ] {
            assert!(host
                .request(
                    "device",
                    true,
                    "files/read",
                    json!({"root":root,"path":"secret.txt"})
                )
                .await
                .is_err());
        }
        assert!(host
            .request(
                "device",
                true,
                "files/read",
                json!({"root":project_id(&fixture.root),"path":"../private/secret.txt"})
            )
            .await
            .is_err());
        assert!(host
            .request(
                "device",
                true,
                "thread/read",
                json!({"threadId":"private-thread"})
            )
            .await
            .is_err());
        let started = host.request("device",true,"thread/start",json!({"cwd":project_id(&fixture.root),"sandbox":"danger-full-access","approvalPolicy":"never","config":{"bad":true}})).await.unwrap();
        assert_eq!(started["received"]["sandbox"], "workspace-write");
        assert_eq!(started["received"]["approvalPolicy"], "on-request");
        assert!(started["received"].get("config").is_none());
        assert_eq!(started["received"]["cwd"], fixture.root);
        assert!(host.request("device",true,"turn/start",json!({"threadId":"shared-thread","input":[{"type":"localImage","path":"/private/secret"}]})).await.is_err());
        host.shutdown().await;
    }
    #[tokio::test]
    async fn control_includes_local_editor_and_approvals_are_answered_once() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        host.request(
            "device-a",
            true,
            "thread/start",
            json!({"cwd":project_id(&fixture.root)}),
        )
        .await
        .unwrap();
        assert!(host.request(LOCAL,false,"files/save",json!({"root":fixture.root,"path":"main.txt","content":"local","expected":"original"})).await.is_err());
        // Another device can observe without stealing control.
        host.request(
            "device-b",
            true,
            "thread/resume",
            json!({"threadId":"shared-thread"}),
        )
        .await
        .unwrap();
        host.request(
            "device-a",
            true,
            "turn/start",
            json!({"threadId":"shared-thread","input":[{"type":"text","text":"hello"}]}),
        )
        .await
        .unwrap();
        let snapshot = host.snapshot(true);
        let approval = snapshot["approvals"][0]["id"].clone();
        assert!(approval.as_str().unwrap().contains(':'));
        assert!(host
            .request(
                "device-b",
                true,
                "approval/respond",
                json!({"id":approval,"result":{"decision":"accept"}})
            )
            .await
            .is_err());
        host.request(
            "device-a",
            true,
            "approval/respond",
            json!({"id":approval,"result":{"decision":"accept"}}),
        )
        .await
        .unwrap();
        assert!(host
            .request(
                "device-a",
                true,
                "approval/respond",
                json!({"id":approval,"result":{"decision":"accept"}})
            )
            .await
            .is_err());
        host.release("device-a");
        host.request(
            LOCAL,
            false,
            "files/save",
            json!({"root":fixture.root,"path":"main.txt","content":"local","expected":"original"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(PathBuf::from(&fixture.root).join("main.txt")).unwrap(),
            "local"
        );
        assert!(host.request(LOCAL,false,"files/save",json!({"root":fixture.root,"path":"main.txt","content":"stale","expected":"original"})).await.is_err());
        host.shutdown().await;
    }
    #[tokio::test]
    async fn replay_and_snapshots_do_not_leak_unshared_events() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        host.remember_threads(&json!({"data":[{"id":"public","cwd":fixture.root},{"id":"private","cwd":fixture.private}]}));
        let before = host.snapshot(false);
        host.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"public","delta":"visible"}}));
        host.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"private","delta":"secret"}}));
        let replay = host
            .request(
                "device",
                true,
                "host/replay",
                json!({"epoch":before["epoch"],"sequence":before["sequence"]}),
            )
            .await
            .unwrap();
        assert_eq!(replay["reset"], false);
        assert_eq!(replay["events"].as_array().unwrap().len(), 1);
        assert!(!replay.to_string().contains("secret"));
        assert!(replay["sequence"].as_u64() > before["sequence"].as_u64());
        assert_eq!(
            host.request(
                "device",
                true,
                "host/replay",
                json!({"epoch":"old","sequence":0})
            )
            .await
            .unwrap()["reset"],
            true
        );
        {
            let mut state = host.runtime.lock().unwrap();
            state.history.pop_front();
        }
        assert_eq!(
            host.request(
                "device",
                true,
                "host/replay",
                json!({"epoch":before["epoch"],"sequence":0})
            )
            .await
            .unwrap()["reset"],
            true
        );
        host.shutdown().await;
    }
    #[tokio::test]
    async fn expired_control_cannot_be_renewed_over_a_new_owner() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        host.claim(&fixture.root, "old-session").unwrap();
        host.runtime
            .lock()
            .unwrap()
            .leases
            .get_mut(&fixture.root)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        host.claim(&fixture.root, "new-session").unwrap();
        host.keepalive("old-session");
        assert!(host.claim(&fixture.root, "old-session").is_err());
        host.release("old-session");
        assert_eq!(
            host.runtime.lock().unwrap().leases[&fixture.root].owner,
            "new-session"
        );
        host.request(
            LOCAL,
            false,
            "host/acquire",
            json!({"project":fixture.root}),
        )
        .await
        .unwrap();
        assert!(host.claim(&fixture.root, "new-session").is_err());
        assert_eq!(
            host.runtime.lock().unwrap().leases[&fixture.root].owner,
            LOCAL
        );
        host.shutdown().await;
    }
    #[tokio::test]
    async fn revocation_cancels_a_mutation_waiting_for_the_host_gate() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let listener = crate::lan::listen(
            host.clone(),
            "127.0.0.1:0",
            "Host".into(),
            vec![fixture.root.clone()],
        )
        .await
        .unwrap();
        let remote = crate::lan::Remote::new(listener.address.clone(), listener.invite());
        remote.connect(|_| {}).await.unwrap();
        let gate = host.gate.lock().await;
        let client = remote.clone();
        let root = project_id(&fixture.root);
        let request = tokio::spawn(async move {
            client.request("files/save",json!({"root":root,"path":"main.txt","content":"revoked","expected":"original"})).await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        listener
            .revoke(listener.status()["devices"][0]["id"].as_str().unwrap())
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        drop(gate);
        assert_eq!(
            std::fs::read_to_string(PathBuf::from(&fixture.root).join("main.txt")).unwrap(),
            "original"
        );
        listener.shutdown().await;
        host.shutdown().await;
    }
}
