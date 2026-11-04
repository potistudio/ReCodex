//! Transport-independent owner of a PC's workspace and agent harness.
use crate::{
    harness::{Harness, HarnessConfig},
    workspace::{self, Workspace},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

pub const LOCAL: &str = "local";

#[derive(Default)]
struct Runtime {
    approvals: HashMap<String, Value>,
    active: HashMap<String, Value>,
    completed: VecDeque<String>,
    thread_roots: HashMap<String, String>,
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
    harness: Arc<Harness>,
    // Serialize operation acceptance, including same-chat starts, saves, and approval answers.
    gate: Mutex<()>,
    runtime: StdMutex<Runtime>,
    pub shared: StdMutex<Vec<String>>,
}

pub fn project_id(path: &str) -> String {
    format!("{:x}", Sha256::digest(path.as_bytes()))[..24].to_owned()
}

impl Host {
    pub fn new(directory: PathBuf) -> Result<Arc<Self>, String> {
        Self::build(directory, |path| {
            Harness::new(path, HarnessConfig::from_env())
        })
    }

    #[cfg(test)]
    pub fn new_with_harness(
        directory: PathBuf,
        harness: impl FnOnce(&std::path::Path) -> Arc<Harness>,
    ) -> Result<Arc<Self>, String> {
        Self::build(directory, harness)
    }

    fn build(
        directory: PathBuf,
        harness: impl FnOnce(&std::path::Path) -> Arc<Harness>,
    ) -> Result<Arc<Self>, String> {
        let workspace = Workspace::default();
        workspace::projects_load(&directory, &workspace)?;
        let (events, _) = broadcast::channel(2048);
        let harness = harness(&directory.join("harness.sqlite3"));
        let host = Arc::new(Self {
            lan: crate::lan_store::LanStore::load(&directory)?,
            directory,
            workspace,
            events,
            harness: harness.clone(),
            gate: Mutex::new(()),
            runtime: StdMutex::new(Runtime {
                epoch: Uuid::new_v4().to_string(),
                ..Runtime::default()
            }),
            shared: StdMutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&host);
        harness.set_emit(Arc::new(move |event| {
            if let Some(host) = weak.upgrade() {
                host.publish(event);
            }
        }));
        Ok(host)
    }

    /// The harness runs in this process, so a connection is ready as soon as the host exists.
    /// Provider problems are reported through `provider/list` instead of failing the
    /// connection, which keeps project files available.
    pub async fn connect(self: &Arc<Self>) -> Result<(), String> {
        Ok(())
    }

    fn remember_thread(&self, id: &str, root: &str) {
        self.runtime
            .lock()
            .unwrap()
            .thread_roots
            .insert(id.to_owned(), root.to_owned());
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
        // Approval requests carry their ID; answers are routed back through the harness.
        if let Some(id) = event.get("id").and_then(Value::as_str) {
            state.approvals.insert(id.to_owned(), event.clone());
        }
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
                state.approvals.retain(|_, e| {
                    e["params"]["threadId"] != thread
                        || e["params"]["turnId"] != event["params"]["turn"]["id"]
                });
            }
            "serverRequest/resolved" => {
                if let Some(id) = event["params"]["requestId"].as_str() {
                    state.approvals.remove(id);
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
            || (method == "recodex/fileChanged"
                && self
                    .shared
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|root| event["params"]["projectId"] == project_id(root)))
            || matches!(
                method,
                "recodex/disconnected" | "recodex/providerStatus" | "serverRequest/resolved"
            )
    }
    pub fn remote_event(&self, envelope: &Value) -> Option<Value> {
        self.remote_event_allowed(&self.runtime.lock().unwrap(), envelope)
            .then(|| envelope.clone())
    }
    pub fn snapshot(&self, remote: bool) -> Value {
        self.snapshot_locked(&self.runtime.lock().unwrap(), remote)
    }
    fn snapshot_locked(&self, state: &Runtime, remote: bool) -> Value {
        json!({"epoch":state.epoch,"sequence":state.sequence,
            "approvals":state.approvals.values().filter(|e| !remote || e["params"]["threadId"].as_str().is_some_and(|id| self.is_shared(state,id))).collect::<Vec<_>>(),
            "activeTurns":state.active.iter().filter(|(id,_)| !remote || self.is_shared(state,id)).collect::<HashMap<_,_>>()})
    }

    fn find_root(&self, key: &str, remote: bool) -> Result<String, String> {
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
        Ok(project.path.clone())
    }

    /// Resolve a caller's project key. Local callers keep the path they named (it may be a
    /// previous location of a relocated project); remote callers get the current root.
    fn root(&self, key: &str, remote: bool) -> Result<String, String> {
        let root = self.find_root(key, remote)?;
        Ok(if remote { root } else { key.into() })
    }

    fn project_key(&self, root: &str, remote: bool) -> String {
        if remote {
            project_id(root)
        } else {
            root.to_owned()
        }
    }

    /// The registered project root of a chat, checked against the caller's access.
    fn thread_root(&self, id: &str, remote: bool) -> Result<String, String> {
        if !Harness::is_thread(id) {
            return Err("Unknown chat".into());
        }
        let cwd = self
            .harness
            .thread_root(id)?
            .ok_or("This chat no longer exists")?;
        let root = self.find_root(&self.project_key(&cwd, remote), remote)?;
        self.remember_thread(id, &cwd);
        Ok(root)
    }

    async fn provider_status(&self) -> Vec<Value> {
        vec![json!({
            "id":crate::harness::PROVIDER,
            "label":crate::harness::PROVIDER_LABEL,
            "available":self.harness.available(),
            "error":self.harness.error(),
            "modelCount":self.harness.cached_model_count().await
        })]
    }

    /// An unconfigured provider yields an empty catalog so the workspace still connects; the
    /// reason is reported through `provider/list`.
    async fn model_list(&self) -> Result<Value, String> {
        let mut data = if self.harness.available() {
            self.harness.models().await?
        } else {
            Vec::new()
        };
        if let Some(first) = data.first_mut() {
            first["isDefault"] = json!(true);
        }
        Ok(json!({"data":data,"nextCursor":Value::Null}))
    }

    fn start_thread(&self, params: &Value, root: &str) -> Result<Value, String> {
        let model = params["model"].as_str().filter(|model| !model.is_empty());
        let id = format!("{}{}", crate::harness::THREAD_PREFIX, Uuid::new_v4());
        let thread = self
            .harness
            .start_session(&id, root, model, params["effort"].as_str())?;
        self.remember_thread(&id, root);
        self.publish(json!({"method":"thread/started","params":{"thread":thread.clone()}}));
        Ok(json!({
            "thread":thread,
            "model":thread["model"],
            "reasoningEffort":params["effort"].as_str().unwrap_or("off")
        }))
    }

    fn thread_action(
        &self,
        id: &str,
        method: &str,
        params: &Value,
        remote: bool,
    ) -> Result<Value, String> {
        self.thread_root(id, remote)?;
        match method {
            "thread/read" => Ok(json!({"thread":self.harness.read_thread(id)?["thread"]})),
            "thread/resume" => self.harness.read_thread(id),
            "turn/start" => {
                let text = text_input(&params["input"])?;
                let result = self.harness.turn(
                    id,
                    &text,
                    params["model"].as_str(),
                    params["effort"].as_str(),
                )?;
                let mut state = self.runtime.lock().unwrap();
                let turn = &result["turn"];
                if !state
                    .completed
                    .iter()
                    .any(|completed| Some(completed.as_str()) == turn["id"].as_str())
                {
                    state.active.insert(id.to_owned(), turn.clone());
                }
                Ok(result)
            }
            "turn/interrupt" => {
                let turn_id = params["turnId"].as_str().ok_or("Missing turnId")?;
                self.harness.interrupt(id, turn_id)?;
                Ok(Value::Null)
            }
            "thread/delete" => {
                self.harness.delete(id)?;
                self.publish(json!({"method":"thread/deleted","params":{"threadId":id}}));
                self.runtime.lock().unwrap().thread_roots.remove(id);
                Ok(Value::Null)
            }
            _ => Err(format!("Operation {method} is not available for a chat")),
        }
    }

    pub async fn request(
        self: &Arc<Self>,
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
                        workspace::file_save(
                            &self.workspace,
                            root.clone(),
                            path.clone(),
                            string("content")?,
                            string("expected")?,
                        )?;
                        self.publish(json!({"method":"recodex/fileChanged","params":{"projectId":project_id(&root),"cwd":root,"path":path}}));
                        Ok(Value::Null)
                    }
                }
            }
            "model/list" => self.model_list().await,
            "provider/list" => Ok(json!(self.provider_status().await)),
            "thread/list" => {
                let root = self.root(&string("cwd")?, remote)?;
                let mut data = self.harness.list_threads(&root).await?;
                if let Some(limit) = params["limit"].as_u64() {
                    data.truncate(limit.min(100) as usize);
                }
                Ok(json!({"data":data,"nextCursor":Value::Null}))
            }
            "thread/start" => {
                let root = self.find_root(&string("cwd")?, remote)?;
                self.start_thread(&params, &root)
            }
            "thread/read" | "thread/resume" | "thread/delete" | "turn/start" | "turn/interrupt" => {
                let id = string("threadId")?;
                if method == "turn/start" {
                    text_input(&params["input"])?;
                }
                if method == "turn/start" && self.runtime.lock().unwrap().active.contains_key(&id) {
                    return Err("This chat already has a running turn".into());
                }
                if method == "thread/delete"
                    && self.runtime.lock().unwrap().active.contains_key(&id)
                {
                    return Err("Stop the running turn before deleting this chat".into());
                }
                self.thread_action(&id, method, &params, remote)
            }
            "approval/respond" => {
                let id = string("id")?;
                let event = self
                    .runtime
                    .lock()
                    .unwrap()
                    .approvals
                    .get(&id)
                    .cloned()
                    .ok_or("This request has already been resolved")?;
                let thread = event["params"]["threadId"]
                    .as_str()
                    .ok_or("This request has no chat")?;
                self.thread_root(thread, remote)?;
                self.harness.respond(&id, &params["result"])?;
                self.runtime.lock().unwrap().approvals.remove(&id);
                self.publish(json!({"method":"serverRequest/resolved","params":{"requestId":id}}));
                Ok(Value::Null)
            }
            _ => Err(format!("Operation {method} is not available on this host")),
        }
    }

    pub async fn shutdown(&self) {
        self.harness.shutdown();
    }
}

fn text_input(value: &Value) -> Result<String, String> {
    let inputs = value.as_array().ok_or("A text message is required")?;
    if inputs.is_empty()
        || inputs.len() > 32
        || inputs.iter().any(|input| {
            input["type"] != "text"
                || input["text"]
                    .as_str()
                    .is_none_or(|text| text.len() > 1024 * 1024)
        })
    {
        return Err("Only text messages up to 1 MB are supported".into());
    }
    Ok(inputs
        .iter()
        .filter_map(|input| input["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n"))
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
    /// A host with a public (shared) and a private project whose model asks for one command
    /// approval per turn.
    pub async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("recodex-host-{}", Uuid::new_v4()));
        let root = directory.join("public");
        let private = directory.join("private");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(root.join("main.txt"), "original").unwrap();
        std::fs::write(private.join("secret.txt"), "secret").unwrap();
        let host = Host::new_with_harness(
            directory.join("data"),
            crate::harness::testing::approval_harness,
        )
        .unwrap();
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
        Self {
            host,
            root,
            private,
            directory,
        }
    }

    /// Start a chat in `root` as a local caller.
    pub async fn thread(&self, root: &str) -> String {
        self.host
            .request(
                false,
                "thread/start",
                json!({"cwd":root,"model":"recodex/test-model"}),
            )
            .await
            .unwrap()["thread"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
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
    use std::time::Duration;

    async fn next(events: &mut broadcast::Receiver<Value>, method: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let envelope = events.recv().await.unwrap();
                if envelope["event"]["method"] == method {
                    return envelope["event"].clone();
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {method}"))
    }

    fn text(value: &str) -> Value {
        json!([{"type":"text","text":value}])
    }

    #[tokio::test]
    async fn published_roots_paths_and_method_policy_are_enforced() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let list = host
            .request(true, "projects/list", json!({}))
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
            assert!(host.request(true, method, json!({})).await.is_err());
        }
        for root in [
            project_id(&fixture.private),
            fixture.root.clone(),
            "unknown".into(),
        ] {
            assert!(host
                .request(true, "files/read", json!({"root":root,"path":"secret.txt"}))
                .await
                .is_err());
        }
        assert!(host
            .request(
                true,
                "files/read",
                json!({"root":project_id(&fixture.root),"path":"../private/secret.txt"})
            )
            .await
            .is_err());
        let private = fixture.thread(&fixture.private).await;
        for method in ["thread/read", "thread/resume", "thread/delete"] {
            assert!(host
                .request(true, method, json!({"threadId":private}))
                .await
                .is_err());
        }
        assert!(host
            .request(true, "thread/read", json!({"threadId":"unknown-thread"}))
            .await
            .is_err());
        assert!(host
            .request(
                true,
                "thread/start",
                json!({"cwd":project_id(&fixture.private)})
            )
            .await
            .is_err());
        let started = host
            .request(
                true,
                "thread/start",
                json!({"cwd":project_id(&fixture.root),"model":"recodex/test-model"}),
            )
            .await
            .unwrap();
        assert_eq!(started["thread"]["cwd"], fixture.root);
        let shared = started["thread"]["id"].as_str().unwrap();
        let listed = host
            .request(
                true,
                "thread/list",
                json!({"cwd":project_id(&fixture.root)}),
            )
            .await
            .unwrap();
        assert_eq!(listed["data"].as_array().unwrap().len(), 1);
        assert!(host
            .request(
                true,
                "turn/start",
                json!({"threadId":shared,"input":[{"type":"localImage","path":"/private/secret"}]})
            )
            .await
            .is_err());
        host.shutdown().await;
    }
    #[tokio::test]
    async fn replay_and_snapshots_do_not_leak_unshared_events() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        host.remember_thread("public", &fixture.root);
        host.remember_thread("private", &fixture.private);
        let before = host.snapshot(false);
        host.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"public","delta":"visible"}}));
        host.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"private","delta":"secret"}}));
        let replay = host
            .request(
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
            host.request(true, "host/replay", json!({"epoch":"old","sequence":0}))
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
        let gate = host.gate.try_lock().expect("test owns the host gate");
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
    #[tokio::test]
    async fn clients_can_save_without_handoff_and_stale_concurrent_saves_fail() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let mut events = host.events.subscribe();
        let (local, remote) = tokio::join!(
            host.request(false,"files/save",json!({"root":fixture.root,"path":"main.txt","content":"local","expected":"original"})),
            host.request(true,"files/save",json!({"root":project_id(&fixture.root),"path":"main.txt","content":"remote","expected":"original"})),
        );
        assert_eq!(usize::from(local.is_ok()) + usize::from(remote.is_ok()), 1);
        let changed = events.recv().await.unwrap();
        assert_eq!(changed["event"]["method"], "recodex/fileChanged");
        assert!(host.remote_event(&changed).is_some());
        assert!(events.try_recv().is_err());
        let current =
            workspace::file_read(&host.workspace, fixture.root.clone(), "main.txt".into()).unwrap();
        host.request(false,"files/save",json!({"root":fixture.root,"path":"main.txt","content":"local again","expected":current})).await.unwrap();
        host.request(true,"files/save",json!({"root":project_id(&fixture.root),"path":"main.txt","content":"remote again","expected":"local again"})).await.unwrap();
        host.request(false,"files/save",json!({"root":fixture.private,"path":"secret.txt","content":"still private","expected":"secret"})).await.unwrap();
        events.recv().await.unwrap();
        events.recv().await.unwrap();
        let private = events.recv().await.unwrap();
        assert!(host.remote_event(&private).is_none());
        host.shutdown().await;
    }
    #[tokio::test]
    async fn approvals_are_answered_once_across_clients_without_handoff() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let thread = fixture.thread(&fixture.root).await;
        let mut events = host.events.subscribe();
        host.request(
            false,
            "turn/start",
            json!({"threadId":thread,"input":text("run")}),
        )
        .await
        .unwrap();
        let approval =
            next(&mut events, "item/commandExecution/requestApproval").await["id"].clone();
        assert_eq!(host.snapshot(true)["approvals"][0]["id"], approval);
        let (local, remote) = tokio::join!(
            host.request(
                false,
                "approval/respond",
                json!({"id":approval,"result":{"decision":"accept"}})
            ),
            host.request(
                true,
                "approval/respond",
                json!({"id":approval,"result":{"decision":"decline"}})
            ),
        );
        assert_eq!(usize::from(local.is_ok()) + usize::from(remote.is_ok()), 1);
        assert!(host.snapshot(true)["approvals"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            next(&mut events, "serverRequest/resolved").await["params"]["requestId"],
            approval
        );
        next(&mut events, "turn/completed").await;
        host.shutdown().await;
    }
    #[tokio::test]
    async fn same_chat_starts_serialize_while_other_chats_and_cross_client_stop_work() {
        let fixture = Fixture::new().await;
        let host = &fixture.host;
        let shared = fixture.thread(&fixture.root).await;
        let other = fixture.thread(&fixture.root).await;
        let mut events = host.events.subscribe();
        let message = json!({"threadId":shared,"input":text("run")});
        let (local, remote) = tokio::join!(
            host.request(false, "turn/start", message.clone()),
            host.request(true, "turn/start", message)
        );
        assert_eq!(usize::from(local.is_ok()) + usize::from(remote.is_ok()), 1);
        let turn = local.as_ref().or(remote.as_ref()).unwrap()["turn"]["id"].clone();
        let observed = host
            .request(true, "thread/resume", json!({"threadId":shared}))
            .await
            .unwrap();
        assert_eq!(observed["thread"]["turns"][0]["status"], "inProgress");
        host.request(
            true,
            "turn/start",
            json!({"threadId":other,"input":text("another chat")}),
        )
        .await
        .unwrap();
        assert_eq!(
            host.snapshot(true)["activeTurns"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert!(host
            .request(true, "thread/delete", json!({"threadId":shared}))
            .await
            .is_err());
        host.request(
            true,
            "turn/interrupt",
            json!({"threadId":shared,"turnId":turn}),
        )
        .await
        .unwrap();
        loop {
            let completed = next(&mut events, "turn/completed").await;
            if completed["params"]["threadId"] == shared {
                assert_eq!(completed["params"]["turn"]["status"], "interrupted");
                break;
            }
        }
        assert!(host.snapshot(true)["activeTurns"].get(&shared).is_none());
        assert!(host.snapshot(true)["activeTurns"].get(&other).is_some());
        host.request(
            true,
            "turn/start",
            json!({"threadId":shared,"input":text("continue")}),
        )
        .await
        .unwrap();
        host.shutdown().await;
    }
}
