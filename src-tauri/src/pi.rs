//! Pi RPC provider adapter.
//!
//! Pi is executed as a child process in `--mode rpc` mode. This module owns the
//! JSONL protocol, session lifecycle, event translation, and the normalization
//! of Pi's model catalog into the model shape used by the rest of ReCodex.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::BufRead,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
};
use uuid::Uuid;

const MODEL_PREFIX: &str = "pi/";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const CATALOG_TTL: Duration = Duration::from_secs(60);
const THINKING_LEVELS: [(&str, &str); 7] = [
    ("off", "No reasoning"),
    ("minimal", "Minimal reasoning"),
    ("low", "Faster responses"),
    ("medium", "Balanced reasoning"),
    ("high", "More thorough reasoning"),
    ("xhigh", "Extra high reasoning"),
    ("max", "Maximum reasoning"),
];

type Pending = Arc<StdMutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>>;
type Emit = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct PiCommand {
    program: String,
    prefix_args: Vec<String>,
}

impl PiCommand {
    pub fn resolve() -> Result<Self, String> {
        if let Ok(value) = std::env::var("RECODEX_PI_PATH") {
            let path = PathBuf::from(&value);
            if path.exists() {
                return Self::from_path(path);
            }
            if !value.contains('/') && !value.contains('\\') && !value.contains('.') {
                return Ok(Self {
                    program: value,
                    prefix_args: Vec::new(),
                });
            }
            return Err(format!("RECODEX_PI_PATH does not exist: {value}"));
        }
        let path = std::env::var_os("PATH")
            .ok_or("Pi CLI was not found. Install Pi or set RECODEX_PI_PATH.")?;
        let names: &[&str] = if cfg!(windows) {
            &["pi.exe", "pi.cmd", "pi.bat", "pi"]
        } else {
            &["pi"]
        };
        for directory in std::env::split_paths(&path) {
            for name in names {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Self::from_path(candidate);
                }
            }
        }
        Err("Pi CLI was not found. Install Pi or set RECODEX_PI_PATH.".into())
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        let text = path.to_string_lossy().into_owned();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "js" | "mjs" | "cjs") {
            return Ok(Self {
                program: "node".into(),
                prefix_args: vec![text],
            });
        }
        if cfg!(windows) && matches!(extension.as_str(), "cmd" | "bat") {
            return Ok(Self {
                program: "cmd".into(),
                prefix_args: vec!["/C".into(), text],
            });
        }
        Ok(Self {
            program: text,
            prefix_args: Vec::new(),
        })
    }

    #[cfg(test)]
    pub fn node(script: impl Into<PathBuf>) -> Self {
        Self {
            program: "node".into(),
            prefix_args: vec![script.into().to_string_lossy().into_owned()],
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args);
        command
    }
}

pub struct PiProvider {
    command: Result<PiCommand, String>,
    session_dir: PathBuf,
    sessions: Mutex<HashMap<String, Arc<PiSession>>>,
    catalog: Mutex<Option<CatalogCache>>,
    emit: StdMutex<Option<Emit>>,
}

struct CatalogCache {
    at: Instant,
    models: Vec<Value>,
}

impl PiProvider {
    pub fn new(session_dir: PathBuf, command: Result<PiCommand, String>) -> Self {
        Self {
            command,
            session_dir,
            sessions: Mutex::new(HashMap::new()),
            catalog: Mutex::new(None),
            emit: StdMutex::new(None),
        }
    }

    pub fn set_emit(&self, emit: Emit) {
        *self.emit.lock().unwrap() = Some(emit);
    }

    fn emit_callback(&self) -> Option<Emit> {
        self.emit.lock().unwrap().clone()
    }

    pub fn available(&self) -> bool {
        self.command.is_ok()
    }

    pub fn error(&self) -> Option<String> {
        self.command.as_ref().err().cloned()
    }

    pub async fn cached_model_count(&self) -> usize {
        self.catalog
            .lock()
            .await
            .as_ref()
            .map(|cache| cache.models.len())
            .unwrap_or(0)
    }

    pub async fn models(&self) -> Result<Vec<Value>, String> {
        {
            let cache = self.catalog.lock().await;
            if let Some(cache) = cache.as_ref() {
                if cache.at.elapsed() < CATALOG_TTL {
                    return Ok(cache.models.clone());
                }
            }
        }
        let command = self.command.as_ref().map_err(Clone::clone)?;
        let emit = self.emit_callback();
        let log = move |line: String| {
            if let Some(emit) = &emit {
                emit(json!({"method":"recodex/log","params":{"message":line}}));
            }
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let rpc = RpcProcess::spawn(
            command,
            None,
            &[
                "--mode".to_owned(),
                "rpc".to_owned(),
                "--no-session".to_owned(),
                "--no-tools".to_owned(),
                "--no-approve".to_owned(),
            ],
            move |event| {
                let _ = sender.send(event);
            },
            log,
        )
        .await?;
        let result = rpc.request("get_available_models", json!({})).await;
        rpc.close().await;
        let result = result?;
        let models = normalize_models(
            result["data"]["models"]
                .as_array()
                .or_else(|| result["models"].as_array())
                .cloned()
                .unwrap_or_default(),
        );
        *self.catalog.lock().await = Some(CatalogCache {
            at: Instant::now(),
            models: models.clone(),
        });
        while receiver.try_recv().is_ok() {}
        Ok(models)
    }

    pub async fn start_session(
        &self,
        id: &str,
        cwd: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Arc<PiSession>, String> {
        let (running, stale) = {
            let mut sessions = self.sessions.lock().await;
            match sessions.get(id) {
                Some(session) if session.rpc.is_running() => (Some(session.clone()), None),
                Some(_) => (None, sessions.remove(id)),
                None => (None, None),
            }
        };
        if let Some(stale) = stale {
            stale.rpc.close().await;
        }
        if let Some(session) = running {
            if let Some(model) = model {
                session.set_model(model).await?;
            }
            if let Some(effort) = effort {
                session.set_effort(effort).await?;
            }
            return Ok(session);
        }
        let command = self.command.as_ref().map_err(Clone::clone)?;
        let mut args = vec![
            "--mode".to_owned(),
            "rpc".to_owned(),
            "--session-id".to_owned(),
            id.to_owned(),
            "--session-dir".to_owned(),
            self.session_dir.to_string_lossy().into_owned(),
        ];
        if std::env::var("RECODEX_PI_TRUST_PROJECTS").as_deref() == Ok("1") {
            args.push("--approve".into());
        } else {
            args.push("--no-approve".into());
        }
        if let Some(model) = model {
            let model = PiModelRef::parse(model)?;
            args.extend([
                "--provider".into(),
                model.provider,
                "--model".into(),
                model.model_id,
            ]);
        }
        if let Some(effort) = effort {
            if !effort.is_empty() {
                args.extend(["--thinking".into(), effort.to_owned()]);
            }
        }
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let emit = self.emit_callback();
        let log_emit = emit.clone();
        let rpc = RpcProcess::spawn(
            command,
            Some(cwd),
            &args,
            move |event| {
                let _ = sender.send(event);
            },
            move |line| {
                if let Some(emit) = &log_emit {
                    emit(json!({"method":"recodex/log","params":{"message":line}}));
                }
            },
        )
        .await?;
        let session = Arc::new(PiSession {
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            rpc: rpc.clone(),
            emit: emit.unwrap_or_else(|| Arc::new(|_| {})),
            state: StdMutex::new(PiState::default()),
        });
        self.sessions
            .lock()
            .await
            .insert(id.to_owned(), session.clone());
        let weak = Arc::downgrade(&session);
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                session.handle_event(event);
            }
        });
        match tokio::time::timeout(START_TIMEOUT, session.get_state()).await {
            Ok(Ok(state)) => {
                session.apply_state(&state);
                if let Ok(stats) = session.rpc.request("get_session_stats", json!({})).await {
                    session.apply_stats(&stats);
                }
                Ok(session)
            }
            Ok(Err(error)) => {
                self.sessions.lock().await.remove(id);
                rpc.close().await;
                Err(error)
            }
            Err(_) => {
                self.sessions.lock().await.remove(id);
                rpc.close().await;
                Err("Pi did not respond to the startup request".into())
            }
        }
    }

    pub async fn thread_root(&self, id: &str) -> Result<Option<String>, String> {
        let Some(path) = self.session_file(id)? else {
            return Ok(None);
        };
        Ok(session_header(&path)?
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    pub async fn list_threads(&self, cwd: &str) -> Result<Vec<Value>, String> {
        let mut files = Vec::new();
        walk_jsonl(&self.session_dir, &mut files);
        let key = cwd_key(cwd);
        let mut threads = Vec::new();
        for path in files {
            let Ok(meta) = session_meta(&path) else {
                continue;
            };
            if cwd_key(&meta.cwd) != key {
                continue;
            }
            threads.push(meta.into_thread());
        }
        threads.sort_by(|left, right| {
            right["updatedAt"]
                .as_u64()
                .unwrap_or_default()
                .cmp(&left["updatedAt"].as_u64().unwrap_or_default())
        });
        Ok(threads)
    }

    pub async fn resume_thread(&self, id: &str, cwd: &str) -> Result<Value, String> {
        let session = self.start_session(id, cwd, None, None).await?;
        session.snapshot().await
    }

    pub async fn delete_thread(&self, id: &str) -> Result<(), String> {
        let session = { self.sessions.lock().await.remove(id) };
        if let Some(session) = session {
            session.rpc.close().await;
        }
        let mut files = Vec::new();
        walk_jsonl(&self.session_dir, &mut files);
        for path in files {
            let matches_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("_{id}.jsonl")));
            let matches_header = session_header(&path)
                .ok()
                .and_then(|header| header.get("id").and_then(Value::as_str).map(str::to_owned))
                .is_some_and(|header_id| header_id == id);
            if matches_name || matches_header {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        let sessions: Vec<_> = self.sessions.lock().await.drain().map(|(_, v)| v).collect();
        for session in sessions {
            session.rpc.close().await;
        }
    }

    fn session_file(&self, id: &str) -> Result<Option<PathBuf>, String> {
        let mut files = Vec::new();
        walk_jsonl(&self.session_dir, &mut files);
        for path in &files {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("_{id}.jsonl")))
            {
                return Ok(Some(path.clone()));
            }
        }
        for path in files {
            if session_header(&path)
                .ok()
                .and_then(|header| header.get("id").and_then(Value::as_str).map(str::to_owned))
                .is_some_and(|header_id| header_id == id)
            {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }
}

pub struct PiSession {
    id: String,
    cwd: String,
    rpc: Arc<RpcProcess>,
    emit: Emit,
    state: StdMutex<PiState>,
}

#[derive(Default)]
struct PiState {
    model: Option<Value>,
    effort: String,
    turn: Option<PiTurn>,
    usage_total: UsageTotals,
    next_item: u64,
}

#[derive(Clone, Default)]
struct UsageTotals {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

struct PiTurn {
    id: String,
    message_id: Option<String>,
    error: Option<String>,
    aborted: bool,
    tools: HashMap<String, ToolMeta>,
}

struct ToolMeta {
    item_id: String,
    name: String,
    args: Value,
}

impl PiTurn {
    fn new(id: String) -> Self {
        Self {
            id,
            message_id: None,
            error: None,
            aborted: false,
            tools: HashMap::new(),
        }
    }
}

impl PiSession {
    pub fn model_key(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        state.model.as_ref().and_then(model_key_from_value)
    }

    pub fn effort(&self) -> String {
        self.state.lock().unwrap().effort.clone()
    }

    fn emit(&self, event: Value) {
        (self.emit)(event);
    }

    async fn get_state(&self) -> Result<Value, String> {
        self.rpc.request("get_state", json!({})).await
    }

    fn apply_state(&self, state: &Value) {
        let mut local = self.state.lock().unwrap();
        if state.get("model").is_some_and(Value::is_object) {
            local.model = Some(state["model"].clone());
        }
        if let Some(level) = state["thinkingLevel"].as_str() {
            local.effort = level.to_owned();
        }
    }

    fn apply_stats(&self, stats: &Value) {
        let mut local = self.state.lock().unwrap();
        let tokens = &stats["tokens"];
        local.usage_total = UsageTotals {
            input: number(tokens, "input"),
            cached: number(tokens, "cacheRead"),
            cache_write: number(tokens, "cacheWrite"),
            output: number(tokens, "output"),
            reasoning: 0,
            total: number(tokens, "total"),
        };
    }

    async fn set_model(&self, key: &str) -> Result<(), String> {
        let parsed = PiModelRef::parse(key)?;
        {
            let state = self.state.lock().unwrap();
            if state.model.as_ref().is_some_and(|model| {
                model["provider"].as_str() == Some(&parsed.provider)
                    && model["id"].as_str() == Some(&parsed.model_id)
            }) {
                return Ok(());
            }
        }
        let model = self
            .rpc
            .request(
                "set_model",
                json!({"provider": parsed.provider, "modelId": parsed.model_id}),
            )
            .await?;
        if model.is_object() {
            self.state.lock().unwrap().model = Some(model);
        }
        Ok(())
    }

    async fn set_effort(&self, effort: &str) -> Result<(), String> {
        if effort.is_empty() {
            return Ok(());
        }
        if self.state.lock().unwrap().effort == effort {
            return Ok(());
        }
        self.rpc
            .request("set_thinking_level", json!({"level": effort}))
            .await?;
        self.state.lock().unwrap().effort = effort.to_owned();
        Ok(())
    }

    pub async fn turn(
        &self,
        text: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Value, String> {
        if text.trim().is_empty() {
            return Err("A text message is required".into());
        }
        if self.state.lock().unwrap().turn.is_some() {
            return Err("This chat already has a running turn".into());
        }
        if let Some(model) = model {
            self.set_model(model).await?;
        }
        if let Some(effort) = effort {
            self.set_effort(effort).await?;
        }
        let turn_id = format!("pi-{}", Uuid::new_v4());
        {
            let mut state = self.state.lock().unwrap();
            if state.turn.is_some() {
                return Err("This chat already has a running turn".into());
            }
            state.turn = Some(PiTurn::new(turn_id.clone()));
        }
        self.emit(json!({"method":"turn/started","params":{"threadId":self.id,"turn":{"id":turn_id,"status":"inProgress","items":[]}}}));
        self.emit(json!({"method":"item/completed","params":{
            "threadId":self.id,
            "turnId":turn_id,
            "item":{"id":format!("{turn_id}-user"),"type":"userMessage","content":[{"type":"text","text":text}]}
        }}));
        match self.rpc.request("prompt", json!({"message": text})).await {
            Ok(data) => {
                if data["disposition"].as_str() == Some("handled") {
                    self.finish_turn(false, None);
                }
                Ok(json!({"turn":{"id":turn_id,"status":"inProgress","items":[]}}))
            }
            Err(error) => {
                self.finish_turn(false, Some(error.clone()));
                Err(error)
            }
        }
    }

    pub async fn interrupt(&self, turn_id: &str) -> Result<Value, String> {
        {
            let mut state = self.state.lock().unwrap();
            match state.turn.as_mut() {
                Some(turn) if turn.id == turn_id => turn.aborted = true,
                Some(_) => return Err("This turn is not running in this chat".into()),
                None => return Err("This chat has no running turn".into()),
            }
        }
        let result = self.rpc.request("abort", json!({})).await;
        match result {
            Ok(_) => {
                self.finish_turn(true, None);
                Ok(Value::Null)
            }
            Err(error) => {
                self.finish_turn(true, Some(error.clone()));
                Err(error)
            }
        }
    }

    pub async fn snapshot(&self) -> Result<Value, String> {
        let messages = self.rpc.request("get_messages", json!({})).await?;
        let state = self.get_state().await?;
        let model = state
            .get("model")
            .and_then(model_key_from_value)
            .or_else(|| self.model_key());
        let effort = state["thinkingLevel"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| self.effort());
        let name = state["sessionName"].as_str();
        let thread = messages_to_thread(
            &self.id,
            &self.cwd,
            name,
            model.as_deref(),
            messages["data"]["messages"]
                .as_array()
                .or_else(|| messages["messages"].as_array())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        Ok(json!({"thread":thread,"model":model,"reasoningEffort":effort}))
    }

    fn handle_event(&self, event: Value) {
        match event["type"].as_str().unwrap_or_default() {
            "__closed" => {
                self.finish_turn(true, Some("Pi process ended unexpectedly".into()));
            }
            "message_start" => self.handle_message_start(&event),
            "message_update" => self.handle_message_update(&event),
            "message_end" => self.handle_message_end(&event),
            "tool_execution_start" => self.handle_tool_start(&event),
            "tool_execution_update" => self.handle_tool_update(&event),
            "tool_execution_end" => self.handle_tool_end(&event),
            "agent_settled" => {
                let aborted = event["aborted"].as_bool().unwrap_or(false);
                self.finish_turn(aborted, None);
            }
            "auto_retry_end" if event["success"] == false => {
                let error = event["finalError"]
                    .as_str()
                    .unwrap_or("Pi retry failed")
                    .to_owned();
                if let Some(turn) = self.state.lock().unwrap().turn.as_mut() {
                    turn.error = Some(error);
                }
            }
            "thinking_level_changed" => {
                if let Some(level) = event["level"].as_str() {
                    self.state.lock().unwrap().effort = level.to_owned();
                }
            }
            "extension_error" => {
                let message = event["error"].as_str().unwrap_or("Pi extension error");
                self.emit(json!({"method":"recodex/log","params":{"message":format!("pi extension: {message}")}}));
            }
            _ => {}
        }
    }

    fn handle_message_start(&self, event: &Value) {
        if event["message"]["role"].as_str() != Some("assistant") {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let next = state.next_item;
        state.next_item += 1;
        if let Some(turn) = state.turn.as_mut() {
            turn.message_id = Some(format!("{}-assistant-{next}", turn.id));
        }
    }

    fn handle_message_update(&self, event: &Value) {
        let update = &event["assistantMessageEvent"];
        let kind = update["type"].as_str().unwrap_or_default();
        let (item_id, delta) = {
            let mut state = self.state.lock().unwrap();
            let next = state.next_item;
            state.next_item += 1;
            let Some(turn) = state.turn.as_mut() else {
                return;
            };
            if turn.message_id.is_none() {
                turn.message_id = Some(format!("{}-assistant-{next}", turn.id));
            }
            let item_id = turn.message_id.clone().unwrap_or_default();
            match kind {
                "text_delta" => (
                    item_id,
                    Some(update["delta"].as_str().unwrap_or_default().to_owned()),
                ),
                "thinking_delta" => (
                    format!("{item_id}-thinking"),
                    Some(update["delta"].as_str().unwrap_or_default().to_owned()),
                ),
                _ => (item_id, None),
            }
        };
        if kind == "thinking_delta" {
            if let Some(delta) = delta {
                self.emit(json!({"method":"item/reasoning/summaryTextDelta","params":{
                    "threadId":self.id,
                    "itemId":item_id,
                    "summaryIndex":0,
                    "delta":delta
                }}));
            }
        } else if let Some(delta) = delta {
            if !delta.is_empty() {
                let turn_id = self.turn_id();
                self.emit(json!({"method":"item/agentMessage/delta","params":{
                    "threadId":self.id,
                    "turnId":turn_id,
                    "itemId":item_id,
                    "delta":delta
                }}));
            }
        }
    }

    fn handle_message_end(&self, event: &Value) {
        let message = &event["message"];
        if message["role"].as_str() != Some("assistant") {
            return;
        }
        let (item_id, text, usage, stop_reason, error_message) = {
            let mut state = self.state.lock().unwrap();
            let next = state.next_item;
            state.next_item += 1;
            let Some(turn) = state.turn.as_mut() else {
                return;
            };
            if turn.message_id.is_none() {
                turn.message_id = Some(format!("{}-assistant-{next}", turn.id));
            }
            let item_id = turn.message_id.clone().unwrap_or_default();
            let text = content_text(&message["content"]);
            let usage = message["usage"].clone();
            let stop_reason = message["stopReason"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let error_message = message["errorMessage"].as_str().map(str::to_owned);
            turn.message_id = None;
            (item_id, text, usage, stop_reason, error_message)
        };
        if !text.is_empty() {
            let turn_id = self.turn_id();
            self.emit(json!({"method":"item/completed","params":{
                "threadId":self.id,
                "turnId":turn_id,
                "item":{"id":item_id,"type":"agentMessage","text":text}
            }}));
        }
        if usage.is_object() {
            self.emit_usage(&usage);
        }
        if matches!(stop_reason.as_str(), "error" | "aborted") {
            let message = error_message.unwrap_or_else(|| {
                if stop_reason == "aborted" {
                    "Pi aborted the response".into()
                } else {
                    "Pi could not complete the response".into()
                }
            });
            let mut state = self.state.lock().unwrap();
            if let Some(turn) = state.turn.as_mut() {
                if stop_reason == "aborted" {
                    turn.aborted = true;
                } else {
                    turn.error = Some(message);
                }
            }
        }
    }

    fn handle_tool_start(&self, event: &Value) {
        let call_id = event["toolCallId"].as_str().unwrap_or_default();
        let name = event["toolName"].as_str().unwrap_or("tool");
        let args = event["args"].clone();
        let (item_id, turn_id) = {
            let mut state = self.state.lock().unwrap();
            let next = state.next_item;
            state.next_item += 1;
            let Some(turn) = state.turn.as_mut() else {
                return;
            };
            let item_id = format!("{}-tool-{next}", turn.id);
            turn.tools.insert(
                call_id.to_owned(),
                ToolMeta {
                    item_id: item_id.clone(),
                    name: name.to_owned(),
                    args: args.clone(),
                },
            );
            (item_id, turn.id.clone())
        };
        let item = tool_item(&item_id, name, &args, "", "inProgress", Value::Null);
        self.emit(json!({"method":"item/started","params":{"threadId":self.id,"turnId":turn_id,"item":item}}));
    }

    fn handle_tool_update(&self, event: &Value) {
        let call_id = event["toolCallId"].as_str().unwrap_or_default();
        let output = content_text(&event["partialResult"]["content"]);
        let (item, turn_id) = {
            let mut state = self.state.lock().unwrap();
            let Some(turn) = state.turn.as_mut() else {
                return;
            };
            let turn_id = turn.id.clone();
            let Some(meta) = turn.tools.get_mut(call_id) else {
                return;
            };
            (
                tool_item(
                    &meta.item_id,
                    &meta.name,
                    &meta.args,
                    &output,
                    "inProgress",
                    Value::Null,
                ),
                turn_id,
            )
        };
        self.emit(json!({"method":"item/updated","params":{"threadId":self.id,"turnId":turn_id,"item":item}}));
    }

    fn handle_tool_end(&self, event: &Value) {
        let call_id = event["toolCallId"].as_str().unwrap_or_default();
        let output = content_text(&event["result"]["content"]);
        let failed = event["isError"].as_bool().unwrap_or(false);
        let (item, turn_id) = {
            let mut state = self.state.lock().unwrap();
            let Some(turn) = state.turn.as_mut() else {
                return;
            };
            let turn_id = turn.id.clone();
            let Some(meta) = turn.tools.remove(call_id) else {
                return;
            };
            (
                tool_item(
                    &meta.item_id,
                    &meta.name,
                    &meta.args,
                    &output,
                    if failed { "failed" } else { "completed" },
                    event["result"].clone(),
                ),
                turn_id,
            )
        };
        self.emit(json!({"method":"item/completed","params":{"threadId":self.id,"turnId":turn_id,"item":item}}));
    }

    fn turn_id(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .turn
            .as_ref()
            .map(|turn| turn.id.clone())
            .unwrap_or_default()
    }

    fn finish_turn(&self, aborted: bool, error: Option<String>) {
        let turn = {
            let mut state = self.state.lock().unwrap();
            let Some(mut turn) = state.turn.take() else {
                return;
            };
            if aborted {
                turn.aborted = true;
            }
            if error.is_some() {
                turn.error = error;
            }
            turn
        };
        let status = if turn.error.is_some() {
            "failed"
        } else if turn.aborted {
            "interrupted"
        } else {
            "completed"
        };
        let mut turn_value = json!({"id":turn.id,"status":status,"items":[]});
        if let Some(error) = &turn.error {
            turn_value["error"] = json!({"message":error});
        }
        self.emit(
            json!({"method":"turn/completed","params":{"threadId":self.id,"turn":turn_value}}),
        );
    }

    fn emit_usage(&self, usage: &Value) {
        let (totals, context) = {
            let mut state = self.state.lock().unwrap();
            let last = usage_totals(usage);
            state.usage_total.input += last.input;
            state.usage_total.cached += last.cached;
            state.usage_total.cache_write += last.cache_write;
            state.usage_total.output += last.output;
            state.usage_total.reasoning += last.reasoning;
            state.usage_total.total += last.total;
            (
                state.usage_total.clone(),
                state
                    .model
                    .as_ref()
                    .and_then(|model| model["contextWindow"].as_u64()),
            )
        };
        let turn_id = self.turn_id();
        self.emit(json!({"method":"thread/tokenUsage/updated","params":{
            "threadId":self.id,
            "turnId":turn_id,
            "tokenUsage":{
                "total": json!({
                    "totalTokens":totals.total,
                    "inputTokens":totals.input,
                    "cachedInputTokens":totals.cached,
                    "cacheWriteInputTokens":totals.cache_write,
                    "outputTokens":totals.output,
                    "reasoningOutputTokens":totals.reasoning
                }),
                "last": json!({
                    "totalTokens":number(usage, "totalTokens"),
                    "inputTokens":number(usage, "input"),
                    "cachedInputTokens":number(usage, "cacheRead"),
                    "cacheWriteInputTokens":number(usage, "cacheWrite"),
                    "outputTokens":number(usage, "output"),
                    "reasoningOutputTokens":number(usage, "reasoning")
                }),
                "modelContextWindow":context
            }
        }}));
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PiModelRef {
    provider: String,
    model_id: String,
}

impl PiModelRef {
    fn parse(value: &str) -> Result<Self, String> {
        let rest = value
            .strip_prefix(MODEL_PREFIX)
            .ok_or_else(|| format!("Model {value} does not belong to the Pi provider"))?;
        let (provider, model_id) = rest
            .split_once('/')
            .ok_or_else(|| format!("Pi model {value} is missing a provider"))?;
        if provider.is_empty() || model_id.is_empty() {
            return Err(format!("Pi model {value} is invalid"));
        }
        Ok(Self {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })
    }
}

fn normalize_models(models: Vec<Value>) -> Vec<Value> {
    models
        .into_iter()
        .filter(|model| model["type"].as_str().is_none_or(|kind| kind == "chat"))
        .map(|model| normalize_model(&model))
        .collect()
}

fn normalize_model(model: &Value) -> Value {
    let provider = model["provider"].as_str().unwrap_or("unknown");
    let id = model["id"].as_str().unwrap_or_default();
    let key = format!("{MODEL_PREFIX}{provider}/{id}");
    let reasoning = model["reasoning"].as_bool().unwrap_or(false);
    let levels = if reasoning {
        THINKING_LEVELS
            .iter()
            .filter(|(level, _)| {
                model
                    .get("thinkingLevelMap")
                    .and_then(|map| map.get(*level))
                    != Some(&Value::Null)
            })
            .map(|(level, description)| json!({"reasoningEffort":level,"description":description}))
            .collect::<Vec<_>>()
    } else {
        vec![json!({"reasoningEffort":"off","description":"No reasoning"})]
    };
    let has_level = |wanted: &str| {
        levels
            .iter()
            .any(|level| level["reasoningEffort"] == wanted)
    };
    let default_reasoning = if !reasoning {
        "off"
    } else if has_level("medium") {
        "medium"
    } else if has_level("low") {
        "low"
    } else if has_level("off") {
        "off"
    } else {
        levels
            .first()
            .and_then(|level| level["reasoningEffort"].as_str())
            .unwrap_or("off")
    };
    json!({
        "id": key,
        "model": key,
        "displayName": model["name"].as_str().unwrap_or(id),
        "description": model["api"].as_str().unwrap_or("Pi"),
        "isDefault": false,
        "hidden": false,
        "defaultReasoningEffort": default_reasoning,
        "supportedReasoningEfforts": levels,
        "provider": "pi",
        "providerLabel": format!("Pi · {provider}"),
        "providerId": provider,
        "providerModel": id,
        "contextWindow": model["contextWindow"],
        "maxTokens": model["maxTokens"],
        "input": model["input"],
        "cost": model["cost"]
    })
}

fn model_key_from_value(model: &Value) -> Option<String> {
    let provider = model["provider"].as_str()?;
    let id = model["id"].as_str()?;
    Some(format!("{MODEL_PREFIX}{provider}/{id}"))
}

fn usage_totals(usage: &Value) -> UsageTotals {
    let input = number(usage, "input");
    let cached = number(usage, "cacheRead");
    let cache_write = number(usage, "cacheWrite");
    let output = number(usage, "output");
    let reasoning = number(usage, "reasoning");
    let total = {
        let total = number(usage, "totalTokens");
        if total == 0 {
            input + cached + cache_write + output
        } else {
            total
        }
    };
    UsageTotals {
        input,
        cached,
        cache_write,
        output,
        reasoning,
        total,
    }
}

fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_default()
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn tool_item(
    item_id: &str,
    name: &str,
    args: &Value,
    output: &str,
    status: &str,
    result: Value,
) -> Value {
    match name {
        "bash" | "powershell" => json!({
            "id": item_id,
            "type": "commandExecution",
            "command": args["command"].as_str().unwrap_or_default(),
            "status": status,
            "aggregatedOutput": output
        }),
        "edit" | "write" => {
            let path = args["path"].as_str().unwrap_or_default();
            let details = &result["details"];
            let mut diff = details["diff"]
                .as_str()
                .or_else(|| details["patch"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if name == "write" {
                        format!(
                            "+{}",
                            args["content"]
                                .as_str()
                                .unwrap_or_default()
                                .lines()
                                .collect::<Vec<_>>()
                                .join("\n+")
                        )
                    } else {
                        let edits = args["edits"].as_array();
                        if let Some(edits) = edits {
                            edits
                                .iter()
                                .filter_map(|edit| {
                                    let old = edit["oldText"].as_str()?;
                                    let new = edit["newText"].as_str()?;
                                    Some(format!("-{old}\n+{new}"))
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            let old = args["oldText"].as_str().unwrap_or_default();
                            let new = args["newText"].as_str().unwrap_or_default();
                            format!("-{old}\n+{new}")
                        }
                    }
                });
            diff.truncate(200_000);
            json!({
                "id": item_id,
                "type": "fileChange",
                "status": status,
                "changes": [{
                    "path": path,
                    "kind": {"type": if name == "write" { "add" } else { "update" }},
                    "diff": diff
                }]
            })
        }
        _ => json!({
            "id": item_id,
            "type": "mcpToolCall",
            "tool": name,
            "server": "pi",
            "status": status,
            "aggregatedOutput": output
        }),
    }
}

#[derive(Clone)]
struct SessionMeta {
    id: String,
    cwd: String,
    name: Option<String>,
    preview: String,
    model: Option<String>,
    updated_at: u64,
}

impl SessionMeta {
    fn into_thread(self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "preview": self.preview,
            "cwd": self.cwd,
            "updatedAt": self.updated_at,
            "turns": [],
            "provider": "pi",
            "providerLabel": "Pi",
            "model": self.model
        })
    }
}

fn session_header(path: &Path) -> Result<Value, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&line).map_err(|error| error.to_string())
}

fn session_meta(path: &Path) -> Result<SessionMeta, String> {
    let header = session_header(path)?;
    if header["type"].as_str() != Some("session") {
        return Err("Not a Pi session".into());
    }
    let id = header["id"]
        .as_str()
        .ok_or("Pi session has no ID")?
        .to_owned();
    let cwd = header["cwd"]
        .as_str()
        .ok_or("Pi session has no working directory")?
        .to_owned();
    let mut name = None;
    let mut preview = String::new();
    let mut model = None;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value["type"].as_str().unwrap_or_default() {
            "session_info" => {
                if let Some(value) = value["name"].as_str() {
                    name = Some(value.to_owned());
                }
            }
            "model_change" => {
                if let (Some(provider), Some(model_id)) =
                    (value["provider"].as_str(), value["modelId"].as_str())
                {
                    model = Some(format!("{MODEL_PREFIX}{provider}/{model_id}"));
                }
            }
            "message" if preview.is_empty() && value["message"]["role"] == "user" => {
                preview = content_text(&value["message"]["content"]);
            }
            _ => {}
        }
    }
    let updated_at = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_else(now_secs);
    Ok(SessionMeta {
        id,
        cwd,
        name,
        preview,
        model,
        updated_at,
    })
}

fn messages_to_thread(
    id: &str,
    cwd: &str,
    name: Option<&str>,
    model: Option<&str>,
    messages: &[Value],
) -> Value {
    let mut turns: Vec<Value> = Vec::new();
    let mut current: Option<usize> = None;
    let mut tools: HashMap<String, (usize, usize)> = HashMap::new();
    for message in messages {
        match message["role"].as_str().unwrap_or_default() {
            "user" => {
                let text = content_text(&message["content"]);
                let turn_id = format!("pi-turn-{}", turns.len() + 1);
                turns.push(json!({
                    "id": turn_id,
                    "status": "completed",
                    "items": [{
                        "id": format!("{turn_id}-user"),
                        "type": "userMessage",
                        "content": [{"type": "text", "text": text}]
                    }]
                }));
                current = Some(turns.len() - 1);
            }
            "assistant" => {
                let index = current.unwrap_or_else(|| {
                    let turn_id = format!("pi-turn-{}", turns.len() + 1);
                    turns.push(json!({"id":turn_id,"status":"completed","items":[]}));
                    current = Some(turns.len() - 1);
                    turns.len() - 1
                });
                let turn_id = turns[index]["id"].as_str().unwrap_or_default().to_owned();
                let text = content_text(&message["content"]);
                if !text.is_empty() {
                    let item_id = format!("{turn_id}-assistant-{}", item_count(&turns[index]));
                    push_item(
                        &mut turns[index],
                        json!({
                            "id": item_id,
                            "type": "agentMessage",
                            "text": text
                        }),
                    );
                }
                if let Some(blocks) = message["content"].as_array() {
                    for block in blocks {
                        if block["type"].as_str() != Some("toolCall") {
                            continue;
                        }
                        let call_id = block["id"].as_str().unwrap_or_default();
                        let item_id = format!("{turn_id}-tool-{}", item_count(&turns[index]));
                        let item = tool_item(
                            &item_id,
                            block["name"].as_str().unwrap_or("tool"),
                            &block["arguments"],
                            "",
                            "inProgress",
                            Value::Null,
                        );
                        let item_index = item_count(&turns[index]);
                        push_item(&mut turns[index], item);
                        tools.insert(call_id.to_owned(), (index, item_index));
                    }
                }
            }
            "toolResult" => {
                let call_id = message["toolCallId"].as_str().unwrap_or_default();
                let output = content_text(&message["content"]);
                let failed = message["isError"].as_bool().unwrap_or(false);
                if let Some((turn_index, item_index)) = tools.get(call_id).copied() {
                    let item = &mut turns[turn_index]["items"][item_index];
                    item["status"] = json!(if failed { "failed" } else { "completed" });
                    match item["type"].as_str().unwrap_or_default() {
                        "commandExecution" | "mcpToolCall" => {
                            item["aggregatedOutput"] = json!(output);
                        }
                        "fileChange" => {
                            if let Some(diff) = message["details"]["diff"]
                                .as_str()
                                .or_else(|| message["details"]["patch"].as_str())
                            {
                                item["changes"][0]["diff"] = json!(diff);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let preview = turns
        .iter()
        .find_map(|turn| {
            turn["items"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["type"] == "userMessage"))
                .map(|item| content_text(&item["content"]))
        })
        .unwrap_or_default();
    json!({
        "id": id,
        "name": name,
        "preview": preview,
        "cwd": cwd,
        "updatedAt": now_secs(),
        "turns": turns,
        "provider": "pi",
        "providerLabel": "Pi",
        "model": model
    })
}

fn push_item(turn: &mut Value, item: Value) {
    if let Some(items) = turn["items"].as_array_mut() {
        items.push(item);
    }
}

fn item_count(turn: &Value) -> usize {
    turn["items"].as_array().map(Vec::len).unwrap_or_default()
}

fn cwd_key(cwd: &str) -> String {
    let normalized = cwd.replace('\\', "/");
    if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn walk_jsonl(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_jsonl(&path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

struct RpcProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    running: Arc<AtomicBool>,
}

impl RpcProcess {
    async fn spawn(
        command: &PiCommand,
        cwd: Option<&str>,
        args: &[String],
        on_event: impl Fn(Value) + Send + Sync + 'static,
        on_log: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let mut process = command.command();
        process
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        #[cfg(windows)]
        process.creation_flags(0x08000000);
        let mut child = process
            .spawn()
            .map_err(|error| format!("Pi could not start: {error}"))?;
        let stdin = child.stdin.take().ok_or("Pi stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("Pi stdout is unavailable")?;
        let stderr = child.stderr.take().ok_or("Pi stderr is unavailable")?;
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::default();
        let running = Arc::new(AtomicBool::new(true));
        let reader_running = running.clone();
        let responses = pending.clone();
        let event_stdin = stdin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message["type"] == "extension_ui_request" {
                    if let Some(response) = extension_response(&message) {
                        let mut stdin = event_stdin.lock().await;
                        if let Ok(mut bytes) = serde_json::to_vec(&response) {
                            bytes.push(b'\n');
                            let _ = stdin.write_all(&bytes).await;
                            let _ = stdin.flush().await;
                        }
                    }
                    continue;
                }
                if message["type"] == "response" {
                    if let Some(id) = message["id"].as_str() {
                        if let Some(sender) = responses.lock().unwrap().remove(id) {
                            let result = if message["success"].as_bool() == Some(false) {
                                Err(message["error"]
                                    .as_str()
                                    .unwrap_or("Pi request failed")
                                    .to_owned())
                            } else {
                                Ok(message["data"].clone())
                            };
                            let _ = sender.send(result);
                        }
                    }
                    continue;
                }
                on_event(message);
            }
            reader_running.store(false, Ordering::Release);
            for (_, sender) in responses.lock().unwrap().drain() {
                let _ = sender.send(Err("Pi connection closed".into()));
            }
            on_event(json!({"type":"__closed"}));
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                on_log(format!("pi: {line}"));
            }
        });
        Ok(Arc::new(Self {
            child: Mutex::new(child),
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            running,
        }))
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    async fn write(&self, message: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        stdin.flush().await.map_err(|error| error.to_string())
    }

    async fn request(&self, command: &str, params: Value) -> Result<Value, String> {
        if !self.is_running() {
            return Err("Pi is not connected".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let mut payload = json!({"id":id,"type":command});
        if let Some(extra) = params.as_object() {
            for (key, value) in extra {
                payload[key.as_str()] = value.clone();
            }
        }
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);
        if let Err(error) = self.write(payload).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, receiver).await;
        self.pending.lock().unwrap().remove(&id);
        response
            .map_err(|_| format!("{command} timed out"))?
            .map_err(|_| "Pi connection closed".to_owned())?
    }

    async fn close(&self) {
        self.running.store(false, Ordering::Release);
        {
            let mut stdin = self.stdin.lock().await;
            let _ = stdin.shutdown().await;
        }
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
}

fn extension_response(message: &Value) -> Option<Value> {
    if !matches!(
        message["method"].as_str().unwrap_or_default(),
        "select" | "confirm" | "input" | "editor"
    ) {
        return None;
    }
    Some(json!({
        "type": "extension_ui_response",
        "id": message["id"],
        "cancelled": true
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_command() -> PiCommand {
        PiCommand::node(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/pi.mjs"
        ))
    }

    async fn provider() -> (
        PiProvider,
        tempfile::TempDir,
        mpsc::UnboundedReceiver<Value>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let provider = PiProvider::new(directory.path().join("sessions"), Ok(fixture_command()));
        let (sender, receiver) = mpsc::unbounded_channel();
        provider.set_emit(Arc::new(move |event| {
            let _ = sender.send(event);
        }));
        (provider, directory, receiver)
    }

    #[test]
    fn model_keys_are_namespaced() {
        let parsed = PiModelRef::parse("pi/anthropic/claude-sonnet-4").unwrap();
        assert_eq!(parsed.provider, "anthropic");
        assert_eq!(parsed.model_id, "claude-sonnet-4");
        assert!(PiModelRef::parse("gpt-5.4").is_err());
    }

    #[test]
    fn catalog_normalizes_provider_and_thinking_levels() {
        let model = normalize_model(&json!({
            "id": "claude-sonnet-4",
            "name": "Claude Sonnet 4",
            "api": "anthropic-messages",
            "provider": "anthropic",
            "reasoning": true,
            "thinkingLevelMap": {"xhigh": null},
            "contextWindow": 200000
        }));
        assert_eq!(model["model"], "pi/anthropic/claude-sonnet-4");
        assert_eq!(model["provider"], "pi");
        assert_eq!(model["providerId"], "anthropic");
        let levels = model["supportedReasoningEfforts"].as_array().unwrap();
        assert!(levels
            .iter()
            .any(|level| level["reasoningEffort"] == "high"));
        assert!(!levels
            .iter()
            .any(|level| level["reasoningEffort"] == "xhigh"));
    }

    #[tokio::test]
    async fn catalog_and_turn_stream_codex_compatible_events() {
        let (provider, _directory, mut events) = provider().await;
        let models = provider.models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["provider"], "pi");
        let session = provider
            .start_session(
                "pi-test-turn",
                std::env::temp_dir().to_str().unwrap(),
                Some("pi/anthropic/claude-sonnet-4"),
                Some("medium"),
            )
            .await
            .unwrap();
        let turn = session
            .turn(
                "hello",
                Some("pi/anthropic/claude-sonnet-4"),
                Some("medium"),
            )
            .await
            .unwrap();
        assert_eq!(turn["turn"]["status"], "inProgress");
        let mut saw_delta = false;
        let mut saw_complete = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                match event["method"].as_str().unwrap_or_default() {
                    "item/agentMessage/delta" => saw_delta = true,
                    "turn/completed" => {
                        assert_eq!(event["params"]["turn"]["status"], "completed");
                        saw_complete = true;
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(saw_delta);
        assert!(saw_complete);
        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot["thread"]["provider"], "pi");
        assert!(!snapshot["thread"]["turns"].as_array().unwrap().is_empty());
        provider.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "Requires an installed and authenticated Pi CLI; performs one small model turn"]
    async fn live_pi_turn() {
        let directory = tempfile::tempdir().unwrap();
        let provider = PiProvider::new(directory.path().join("sessions"), PiCommand::resolve());
        let (sender, mut receiver) = mpsc::unbounded_channel();
        provider.set_emit(Arc::new(move |event| {
            let _ = sender.send(event);
        }));
        let session = provider
            .start_session(
                "pi-live-turn",
                directory.path().to_string_lossy().as_ref(),
                None,
                None,
            )
            .await
            .unwrap();
        session
            .turn(
                "Reply with exactly PI_OK. Do not use any tools.",
                None,
                None,
            )
            .await
            .unwrap();
        let mut answer = String::new();
        let mut saw_usage = false;
        tokio::time::timeout(Duration::from_secs(180), async {
            while let Some(event) = receiver.recv().await {
                match event["method"].as_str().unwrap_or_default() {
                    "item/completed" if event["params"]["item"]["type"] == "agentMessage" => {
                        answer
                            .push_str(event["params"]["item"]["text"].as_str().unwrap_or_default());
                    }
                    "thread/tokenUsage/updated" => saw_usage = true,
                    "turn/completed" => {
                        assert_eq!(event["params"]["turn"]["status"], "completed");
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(!answer.is_empty(), "Pi returned no visible answer");
        assert!(saw_usage, "Pi returned no token usage");
        provider.shutdown().await;
    }

    #[tokio::test]
    async fn session_files_are_listed_and_deleted() {
        let (provider, directory, _events) = provider().await;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let session_dir = directory.path().join("sessions").join("project");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("2024-01-01T00-00-00-000Z_pi-listed.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type":"session","version":3,"id":"pi-listed","timestamp":"2024-01-01T00:00:00.000Z","cwd":cwd}),
                json!({"type":"message","id":"u1","parentId":null,"timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":"Listed chat","timestamp":0}})
            ),
        )
        .unwrap();
        let threads = provider.list_threads(&cwd).await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0]["preview"], "Listed chat");
        assert_eq!(provider.thread_root("pi-listed").await.unwrap(), Some(cwd));
        provider.delete_thread("pi-listed").await.unwrap();
        assert!(!path.exists());
    }
}
