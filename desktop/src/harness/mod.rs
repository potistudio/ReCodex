//! ReCodex's agent harness: the run loop, the session ledger, tool execution, and
//! authorization run inside the host process.
//!
//! - `ledger`: SQLite (WAL) session ledger; the only source of truth for harness chats.
//! - `context`: model-visible messages and UI transcripts projected from ledger events.
//! - `tools`: tool registry, effect-based scheduler, and local execution environment.
//! - `policy`: allow / ask / deny decisions and session grants.
//! - `driver`: the run driver that owns agent invocation, step sequencing, approvals, and
//!   recovery.
//!
//! The harness talks to models only through [`crate::model::ModelClient`]. The client is
//! installed with [`Harness::set_client`]; choosing and building it is not the harness's job.
mod context;
mod driver;
mod ledger;
mod policy;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
mod tools;

use crate::model::ModelClient;
use ledger::Ledger;
use serde_json::{json, Value};
use std::{
	path::Path,
	sync::{
		atomic::{AtomicU64, Ordering},
		Arc, Mutex as StdMutex,
	},
	time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub const PROVIDER: &str = "recodex";
pub const PROVIDER_LABEL: &str = "ReCodex";
pub const THREAD_PREFIX: &str = "rx-";
pub const MODEL_PREFIX: &str = "recodex/";
const DEFAULT_MAX_STEPS: usize = 64;
const CATALOG_TTL: Duration = Duration::from_secs(60);
const EFFORTS: [(&str, &str); 4] = [
	("off", "Do not request reasoning effort"),
	("low", "Low reasoning effort"),
	("medium", "Medium reasoning effort"),
	("high", "High reasoning effort"),
];

type Emit = Arc<dyn Fn(Value) + Send + Sync>;

pub struct Harness {
	ledger: Result<Ledger, String>,
	model: StdMutex<Result<Arc<dyn ModelClient>, String>>,
	/// Label of the active model connection, e.g. "Command Code".
	connection: StdMutex<String>,
	driver: driver::Driver,
	emit: StdMutex<Option<Emit>>,
	catalog: Mutex<Option<(Instant, Vec<Value>, u64)>>,
	/// Bumped on every connection change so a catalog fetched for an old connection is
	/// neither served nor cached.
	connection_generation: AtomicU64,
}

impl Harness {
	/// `path` is the ledger file. The model connection is supplied afterwards through
	/// [`Harness::set_client`].
	pub fn new(path: &Path, max_steps: Option<usize>) -> Arc<Self> {
		let max_steps = max_steps.unwrap_or(DEFAULT_MAX_STEPS).max(1);
		let harness = Arc::new(Self {
			ledger: Ledger::open(path),
			model: StdMutex::new(Err("No model connection is configured".into())),
			connection: StdMutex::new(String::new()),
			driver: driver::Driver::new(max_steps),
			emit: StdMutex::new(None),
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

	#[cfg(test)]
	pub(crate) fn with_client(path: &Path, client: Arc<dyn ModelClient>, max_steps: Option<usize>) -> Arc<Self> {
		let harness = Self::new(path, max_steps);
		harness.set_client(Ok(client), "Test".into());
		harness
	}

	pub fn set_emit(&self, emit: Emit) {
		*self.emit.lock().unwrap() = Some(emit);
	}

	pub fn emit(&self, event: Value) {
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
	pub fn set_client(&self, client: Result<Arc<dyn ModelClient>, String>, label: String) {
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

	/// Accept a prompt and start a run through the driver. Returns once the input is
	/// committed.
	pub fn turn(
		self: &Arc<Self>,
		id: &str,
		text: &str,
		model: Option<&str>,
		effort: Option<&str>,
	) -> Result<Value, String> {
		self.driver.turn(self, id, text, model, effort)
	}

	/// Request cancellation. The run reports `turn/completed` once it has actually stopped.
	pub fn interrupt(&self, id: &str, turn_id: &str) -> Result<(), String> {
		self.driver.interrupt(self, id, turn_id)
	}

	pub fn delete(&self, id: &str) -> Result<(), String> {
		if self.driver.is_running(id) {
			return Err("Stop the running turn before deleting this chat".into());
		}
		self.ledger()?.delete(id)
	}

	/// Deliver an approval answer. Each request accepts exactly one answer.
	pub fn respond(&self, request_id: &str, result: &Value) -> Result<(), String> {
		self.driver.respond(request_id, result)
	}

	pub fn shutdown(&self) {
		self.driver.shutdown();
	}
}

fn now_secs() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|duration| duration.as_secs())
		.unwrap_or_default()
}
