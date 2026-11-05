//! ReCodex's agent harness: the run loop, the session ledger, tool execution, and
//! authorization run inside the host process.
//!
//! - `account`: model connection selection and Command Code sign-in.
//! - `command_code`: Command Code browser sign-in and credential storage.
//! - `ledger`: SQLite (WAL) session ledger; the only source of truth for harness chats.
//! - `context`: model-visible messages and UI transcripts projected from ledger events.
//! - `model`: Model Gateway, shared streaming helpers, and the Chat Completions adapter.
//! - `messages_api`, `responses_api`, `generate_api`: Anthropic Messages, OpenAI Responses,
//!   and Command Code CLI generation adapters.
//! - `command_code_client`: per-model routing and plan fallback for Command Code.
//! - `tools`: tool registry, effect-based scheduler, and local execution environment.
//! - `policy`: allow / ask / deny decisions and session grants.
//! - `driver`: the run driver that sequences steps, attempts, approvals, and tool calls.
mod account;
pub mod command_code;
mod command_code_client;
mod context;
mod driver;
mod generate_api;
mod ledger;
mod messages_api;
mod model;
mod policy;
mod responses_api;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
mod tools;

use ledger::{kind, Ledger};
use model::{ModelClient, OpenAiCompatible};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch, Mutex};
use uuid::Uuid;

pub const PROVIDER: &str = "recodex";
pub const PROVIDER_LABEL: &str = "ReCodex";
pub const THREAD_PREFIX: &str = "rx-";
pub const MODEL_PREFIX: &str = "recodex/";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_STEPS: usize = 64;
const CATALOG_TTL: Duration = Duration::from_secs(60);
const EFFORTS: [(&str, &str); 4] = [
    ("off", "Do not request reasoning effort"),
    ("low", "Low reasoning effort"),
    ("medium", "Medium reasoning effort"),
    ("high", "High reasoning effort"),
];

type Emit = Arc<dyn Fn(Value) + Send + Sync>;

/// Connection settings read from the environment. An explicit OpenAI-compatible endpoint
/// (`RECODEX_OPENAI_BASE_URL` or `RECODEX_OPENAI_API_KEY`) takes precedence over a Command
/// Code sign-in; `OPENAI_API_KEY` is used only when neither is configured.
#[derive(Clone, Default)]
pub struct HarnessConfig {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub models: Option<Vec<String>>,
    pub max_steps: Option<usize>,
}

impl HarnessConfig {
    pub fn from_env() -> Self {
        let var = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            base_url: var("RECODEX_OPENAI_BASE_URL"),
            api_key: var("RECODEX_OPENAI_API_KEY"),
            openai_api_key: var("OPENAI_API_KEY"),
            models: var("RECODEX_OPENAI_MODELS").map(|models| {
                models
                    .split(',')
                    .map(|model| model.trim().to_owned())
                    .filter(|model| !model.is_empty())
                    .collect()
            }),
            max_steps: var("RECODEX_HARNESS_MAX_STEPS").and_then(|value| value.parse().ok()),
        }
    }

    fn explicit(&self) -> bool {
        self.base_url.is_some() || self.api_key.is_some()
    }

    fn openai(&self, api_key: Option<String>) -> Result<Arc<dyn ModelClient>, String> {
        Ok(Arc::new(OpenAiCompatible::new(
            self.base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            api_key,
            self.models.clone(),
        )?))
    }
}

struct ActiveRun {
    run_id: String,
    cancel: watch::Sender<bool>,
}

struct PendingApproval {
    session: String,
    run: String,
    answer: oneshot::Sender<String>,
}

pub struct Harness {
    ledger: Result<Ledger, String>,
    model: StdMutex<Result<Arc<dyn ModelClient>, String>>,
    /// Label of the active model connection, e.g. "Command Code".
    connection: StdMutex<String>,
    account: account::Account,
    max_steps: usize,
    emit: StdMutex<Option<Emit>>,
    runs: StdMutex<HashMap<String, ActiveRun>>,
    approvals: StdMutex<HashMap<String, PendingApproval>>,
    catalog: Mutex<Option<(Instant, Vec<Value>, u64)>>,
    /// Bumped on every connection change so a catalog fetched for an old connection is
    /// neither served nor cached.
    connection_generation: AtomicU64,
}

impl Harness {
    /// `path` is the ledger file; Command Code credentials are stored next to it.
    pub fn new(path: &Path, config: HarnessConfig) -> Arc<Self> {
        let directory = path.parent().unwrap_or(Path::new("."));
        let account = account::Account::new(config, command_code::Store::new(directory));
        let harness = Self::build(path, account, None);
        harness.reconnect();
        harness
    }

    #[cfg(test)]
    pub(crate) fn with_client(
        path: &Path,
        client: Arc<dyn ModelClient>,
        max_steps: Option<usize>,
    ) -> Arc<Self> {
        let harness = Self::build(path, account::Account::disabled(), max_steps);
        harness.set_client(Ok(client), "Test".into());
        harness
    }

    #[cfg(test)]
    pub(crate) fn with_account(path: &Path, account: account::Account) -> Arc<Self> {
        let harness = Self::build(path, account, None);
        harness.reconnect();
        harness
    }

    fn build(path: &Path, account: account::Account, max_steps: Option<usize>) -> Arc<Self> {
        let max_steps = max_steps.or(account.config.max_steps);
        let harness = Arc::new(Self {
            ledger: Ledger::open(path),
            model: StdMutex::new(Err("No model connection is configured".into())),
            connection: StdMutex::new(String::new()),
            account,
            max_steps: max_steps.unwrap_or(DEFAULT_MAX_STEPS).max(1),
            emit: StdMutex::new(None),
            runs: StdMutex::new(HashMap::new()),
            approvals: StdMutex::new(HashMap::new()),
            catalog: Mutex::new(None),
            connection_generation: AtomicU64::new(0),
        });
        if let Ok(ledger) = &harness.ledger {
            if let Err(error) = driver::recover(ledger) {
                eprintln!("ReCodex harness recovery failed: {error}");
            }
        }
        harness
    }

    pub fn set_emit(&self, emit: Emit) {
        *self.emit.lock().unwrap() = Some(emit);
    }

    fn emit(&self, event: Value) {
        let emit = self.emit.lock().unwrap().clone();
        if let Some(emit) = emit {
            emit(event);
        }
    }

    fn ledger(&self) -> Result<&Ledger, String> {
        self.ledger.as_ref().map_err(Clone::clone)
    }

    pub fn is_thread(id: &str) -> bool {
        id.starts_with(THREAD_PREFIX)
    }

    /// The active model client, if a connection is configured.
    fn client(&self) -> Result<Arc<dyn ModelClient>, String> {
        self.model.lock().unwrap().clone()
    }

    /// Replace the model connection. Running steps keep the client they started with.
    fn set_client(&self, client: Result<Arc<dyn ModelClient>, String>, label: String) {
        *self.model.lock().unwrap() = client;
        *self.connection.lock().unwrap() = label;
        self.connection_generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn connection_label(&self) -> String {
        self.connection.lock().unwrap().clone()
    }

    pub fn available(&self) -> bool {
        self.ledger.is_ok() && self.model.lock().unwrap().is_ok()
    }

    pub fn error(&self) -> Option<String> {
        self.ledger
            .as_ref()
            .err()
            .cloned()
            .or_else(|| self.model.lock().unwrap().as_ref().err().cloned())
    }

    pub async fn cached_model_count(&self) -> usize {
        self.catalog
            .lock()
            .await
            .as_ref()
            .map_or(0, |(_, models, _)| models.len())
    }

    pub async fn models(&self) -> Result<Vec<Value>, String> {
        let mut catalog = self.catalog.lock().await;
        let generation = self.connection_generation.load(Ordering::SeqCst);
        if let Some((at, models, cached)) = catalog.as_ref() {
            if *cached == generation && at.elapsed() < CATALOG_TTL {
                return Ok(models.clone());
            }
        }
        let client = self.client()?;
        let label = self.connection_label();
        let models: Vec<Value> = client
            .models()
            .await?
            .into_iter()
            .map(|model| {
                let key = format!("{MODEL_PREFIX}{}", model.id);
                json!({
                    "id": key,
                    "model": key,
                    "displayName": model.id,
                    "description": format!("{label} model run by the ReCodex harness"),
                    "isDefault": false,
                    "hidden": false,
                    "defaultReasoningEffort": "off",
                    "supportedReasoningEfforts": EFFORTS
                        .iter()
                        .map(|(effort, description)| json!({"reasoningEffort":effort,"description":description}))
                        .collect::<Vec<_>>(),
                    "provider": PROVIDER,
                    "providerLabel": label,
                    "providerModel": model.id,
                    "contextWindow": model.context_window
                })
            })
            .collect();
        if self.connection_generation.load(Ordering::SeqCst) == generation {
            *catalog = Some((Instant::now(), models.clone(), generation));
        }
        Ok(models)
    }

    pub async fn list_threads(&self, cwd: &str) -> Result<Vec<Value>, String> {
        Ok(self
            .ledger()?
            .sessions(cwd)?
            .into_iter()
            .map(|session| {
                json!({
                    "id": session.id,
                    "name": session.name,
                    "preview": session.preview,
                    "cwd": session.cwd,
                    "updatedAt": session.updated_at,
                    "turns": [],
                    "provider": PROVIDER,
                    "providerLabel": PROVIDER_LABEL,
                    "model": session.model
                })
            })
            .collect())
    }

    pub fn thread_root(&self, id: &str) -> Result<Option<String>, String> {
        Ok(self.ledger()?.session(id)?.map(|session| session.cwd))
    }

    pub fn start_session(
        &self,
        id: &str,
        cwd: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Value, String> {
        if let Some(model) = model {
            if !model.starts_with(MODEL_PREFIX) {
                return Err("Choose a ReCodex model for this chat".into());
            }
        }
        let effort = effort.filter(|effort| !effort.is_empty());
        self.ledger()?.create_session(
            id,
            cwd,
            model,
            effort,
            json!({"cwd":cwd,"model":model,"effort":effort,"contextVersion":context::VERSION}),
        )?;
        Ok(json!({
            "id": id,
            "name": Value::Null,
            "preview": "",
            "cwd": cwd,
            "updatedAt": now_secs(),
            "turns": [],
            "provider": PROVIDER,
            "providerLabel": PROVIDER_LABEL,
            "model": model
        }))
    }

    pub fn read_thread(&self, id: &str) -> Result<Value, String> {
        let ledger = self.ledger()?;
        let session = ledger.session(id)?.ok_or("This chat no longer exists")?;
        let events = ledger.events(id)?;
        Ok(json!({
            "thread": context::thread(
                id,
                &session.cwd,
                session.name.as_deref(),
                session.model.as_deref(),
                &events
            ),
            "model": session.model,
            "reasoningEffort": session.effort.unwrap_or_else(|| "off".into())
        }))
    }

    /// Accept a prompt, record it, and start a run. Returns once the input is committed.
    pub fn turn(
        self: &Arc<Self>,
        id: &str,
        text: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Value, String> {
        if text.trim().is_empty() {
            return Err("A text message is required".into());
        }
        let ledger = self.ledger()?;
        let session = ledger.session(id)?.ok_or("This chat no longer exists")?;
        let model = model
            .filter(|model| !model.is_empty())
            .map(str::to_owned)
            .or(session.model.clone())
            .ok_or("Choose a model for this chat")?;
        let provider_model = model
            .strip_prefix(MODEL_PREFIX)
            .ok_or("This chat uses the ReCodex harness. Start a new chat to use another provider.")?
            .to_owned();
        let effort = effort
            .filter(|effort| !effort.is_empty())
            .map(str::to_owned)
            .or(session.effort.clone());
        let mut runs = self.runs.lock().unwrap();
        if runs.contains_key(id) {
            return Err("This chat already has a running turn".into());
        }
        if session.model.as_deref() != Some(model.as_str()) || session.effort != effort {
            ledger.append(
                id,
                None,
                kind::SESSION_MODEL,
                json!({"model":model,"effort":effort}),
            )?;
        }
        let input_id = Uuid::new_v4().to_string();
        let run_id = Uuid::new_v4().to_string();
        ledger.append(
            id,
            None,
            kind::INPUT_ACCEPTED,
            json!({"inputId":input_id,"text":text}),
        )?;
        ledger.append(
            id,
            None,
            kind::RUN_STARTED,
            json!({"runId":run_id,"inputId":input_id,"model":model,"effort":effort,"maxSteps":self.max_steps}),
        )?;
        let (cancel, cancelled) = watch::channel(false);
        runs.insert(
            id.to_owned(),
            ActiveRun {
                run_id: run_id.clone(),
                cancel,
            },
        );
        drop(runs);
        let turn = json!({"id":run_id,"status":"inProgress","items":[]});
        self.emit(json!({"method":"turn/started","params":{"threadId":id,"turn":turn}}));
        self.emit(json!({"method":"item/completed","params":{
            "threadId":id,
            "turnId":run_id,
            "item":{"id":format!("{run_id}-user"),"type":"userMessage","content":[{"type":"text","text":text}]}
        }}));
        tokio::spawn(driver::run(
            self.clone(),
            driver::RunSpec {
                session: id.to_owned(),
                run_id,
                cwd: session.cwd,
                model: provider_model,
                effort: effort.filter(|effort| effort != "off"),
                cancel: cancelled,
            },
        ));
        Ok(json!({ "turn": turn }))
    }

    /// Request cancellation. The run reports `turn/completed` once it has actually stopped.
    pub fn interrupt(&self, id: &str, turn_id: &str) -> Result<(), String> {
        let runs = self.runs.lock().unwrap();
        let run = runs.get(id).ok_or("This chat has no running turn")?;
        if run.run_id != turn_id {
            return Err("This turn is not running in this chat".into());
        }
        if !*run.cancel.borrow() {
            self.ledger()?.append(
                id,
                None,
                kind::RUN_CANCEL_REQUESTED,
                json!({"runId":turn_id}),
            )?;
            let _ = run.cancel.send(true);
        }
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if self.runs.lock().unwrap().contains_key(id) {
            return Err("Stop the running turn before deleting this chat".into());
        }
        self.ledger()?.delete(id)
    }

    /// Deliver an approval answer. Each request accepts exactly one answer.
    pub fn respond(&self, request_id: &str, result: &Value) -> Result<(), String> {
        let pending = self
            .approvals
            .lock()
            .unwrap()
            .remove(request_id)
            .ok_or("This request has already been resolved")?;
        let decision = result["decision"]
            .as_str()
            .filter(|decision| {
                matches!(
                    *decision,
                    "accept" | "acceptForSession" | "decline" | "cancel"
                )
            })
            .unwrap_or("decline");
        pending
            .answer
            .send(decision.to_owned())
            .map_err(|_| "This request is no longer waiting for an answer".to_owned())
    }

    pub fn shutdown(&self) {
        for run in self.runs.lock().unwrap().values() {
            let _ = run.cancel.send(true);
        }
    }

    fn register_approval(
        &self,
        request_id: &str,
        session: &str,
        run: &str,
    ) -> oneshot::Receiver<String> {
        let (answer, receiver) = oneshot::channel();
        self.approvals.lock().unwrap().insert(
            request_id.to_owned(),
            PendingApproval {
                session: session.to_owned(),
                run: run.to_owned(),
                answer,
            },
        );
        receiver
    }

    fn release_run(&self, session: &str, run: &str) {
        let mut runs = self.runs.lock().unwrap();
        if runs.get(session).is_some_and(|active| active.run_id == run) {
            runs.remove(session);
        }
        self.approvals
            .lock()
            .unwrap()
            .retain(|_, pending| pending.session != session || pending.run != run);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
