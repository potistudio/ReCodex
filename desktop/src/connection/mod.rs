//! Model connection: chooses the model client for the harness from the environment and the
//! Command Code sign-in, and runs the sign-in flow.
//!
//! This is the one place that knows both the agent harness and the model providers. The
//! harness only receives a [`ModelClient`]; the providers only implement it.
use crate::{
	harness::Harness,
	model::ModelClient,
	provider::{
		command_code::{self, PendingLogin, Source, Store},
		command_code_client::CommandCodeClient,
		gateway::OpenAiCompatible,
	},
};
use serde_json::{json, Value};
use std::{
	path::Path,
	sync::{Arc, Mutex as StdMutex},
};

#[cfg(test)]
mod tests;

pub const COMMAND_CODE: &str = "Command Code";
const OPENAI_COMPATIBLE: &str = "OpenAI-compatible";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Connection settings read from the environment. An explicit OpenAI-compatible endpoint
/// (`RECODEX_OPENAI_BASE_URL` or `RECODEX_OPENAI_API_KEY`) takes precedence over a Command
/// Code sign-in; `OPENAI_API_KEY` is used only when neither is configured.
#[derive(Clone, Default)]
pub struct Config {
	pub base_url: Option<String>,
	pub api_key: Option<String>,
	pub openai_api_key: Option<String>,
	pub models: Option<Vec<String>>,
	pub max_steps: Option<usize>,
}

impl Config {
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
			self.base_url.clone().unwrap_or_else(|| DEFAULT_BASE_URL.into()),
			api_key,
			self.models.clone(),
		)?))
	}
}

/// Opens the harness on `path` (the ledger file) and connects it to a model. Command Code
/// credentials are stored next to the ledger.
pub fn open(path: &Path, config: Config) -> (Arc<Harness>, Arc<Connection>) {
	let directory = path.parent().unwrap_or(Path::new("."));
	let harness = Harness::new(path, config.max_steps);
	let connection = Connection::new(harness.clone(), config, Store::new(directory));
	(harness, connection)
}

pub struct Connection {
	harness: Arc<Harness>,
	config: Config,
	store: Option<Store>,
	studio_url: String,
	api_url: String,
	first_port: u16,
	login: StdMutex<Option<tokio::task::AbortHandle>>,
}

impl Connection {
	fn new(harness: Arc<Harness>, config: Config, store: Store) -> Arc<Self> {
		let connection = Arc::new(Self {
			harness,
			config,
			store: Some(store),
			studio_url: command_code::STUDIO_URL.into(),
			api_url: command_code::API_URL.into(),
			first_port: command_code::FIRST_PORT,
			login: StdMutex::new(None),
		});
		connection.reconnect();
		connection
	}

	/// No credential store and no model selection; used when a test supplies its own model
	/// client to the harness.
	#[cfg(test)]
	pub fn disabled(harness: Arc<Harness>) -> Arc<Self> {
		Arc::new(Self {
			harness,
			config: Config::default(),
			store: None,
			studio_url: String::new(),
			api_url: String::new(),
			first_port: 0,
			login: StdMutex::new(None),
		})
	}

	#[cfg(test)]
	pub fn for_test(harness: Arc<Harness>, config: Config, store: Store, studio_url: &str, api_url: &str) -> Arc<Self> {
		let connection = Arc::new(Self {
			harness,
			config,
			store: Some(store),
			studio_url: studio_url.into(),
			api_url: api_url.into(),
			first_port: 0,
			login: StdMutex::new(None),
		});
		connection.reconnect();
		connection
	}

	/// Choose the model connection: an explicit OpenAI-compatible endpoint, then Command Code
	/// credentials, then `OPENAI_API_KEY`.
	pub fn reconnect(&self) {
		let config = &self.config;
		let credentials = self.store.as_ref().and_then(Store::load);
		let (client, label): (Result<Arc<dyn ModelClient>, String>, &str) = if config.explicit() {
			(config.openai(config.api_key.clone()), OPENAI_COMPATIBLE)
		} else if let Some((credentials, _)) = credentials {
			(
				CommandCodeClient::new(&self.api_url, credentials.api_key, config.models.clone())
					.map(|client| Arc::new(client) as Arc<dyn ModelClient>),
				COMMAND_CODE,
			)
		} else if let Some(key) = &config.openai_api_key {
			(config.openai(Some(key.clone())), OPENAI_COMPATIBLE)
		} else {
			(
				Err("Sign in to Command Code, or set RECODEX_OPENAI_API_KEY or RECODEX_OPENAI_BASE_URL".into()),
				COMMAND_CODE,
			)
		};
		self.harness.set_client(client, label.into());
	}

	/// The Command Code account state shown in settings. Never includes the key itself.
	pub fn account(&self) -> Value {
		let store = self.store.as_ref();
		let pending = self.login.lock().unwrap().is_some();
		let mut account = json!({
			"available": store.is_some(),
			"connection": self.harness.connection_label(),
			"loginPending": pending,
			"signedIn": false
		});
		if let Some((credentials, source)) = store.and_then(Store::load) {
			account["signedIn"] = json!(true);
			account["userName"] = json!(credentials.user_name);
			account["keyName"] = json!(credentials.key_name);
			account["source"] = json!(source.label());
			// Only ReCodex's own sign-in can be removed here.
			account["canSignOut"] = json!(source == Source::ReCodex);
		}
		account
	}

	/// Start a browser sign-in and return the URL to open. The result arrives as an
	/// `account/updated` or `account/login/completed` event.
	pub async fn login_start(self: &Arc<Self>) -> Result<String, String> {
		let store = self.store.clone().ok_or("Command Code sign-in is not available")?;
		self.login_cancel();
		let pending = PendingLogin::start(&self.studio_url, self.first_port).await?;
		let url = pending.auth_url.clone();
		let connection = self.clone();
		let api_url = self.api_url.clone();
		let task = tokio::spawn(async move {
			let result = async {
				let mut credentials = pending.finish(command_code::LOGIN_TIMEOUT).await?;
				let whoami = command_code::validate(&api_url, &credentials.api_key).await?;
				if credentials.user_name.is_empty() {
					if let Some(name) = whoami["user"]["userName"].as_str() {
						credentials.user_name = name.to_owned();
					}
				}
				store.save(&credentials)
			}
			.await;
			*connection.login.lock().unwrap() = None;
			match result {
				Ok(()) => {
					connection.reconnect();
					connection
						.harness
						.emit(json!({"method":"account/updated","params":{"success":true}}));
				}
				Err(error) => connection.harness.emit(json!({
					"method":"account/login/completed",
					"params":{"success":false,"error":{"message":error}}
				})),
			}
		});
		*self.login.lock().unwrap() = Some(task.abort_handle());
		Ok(url)
	}

	pub fn login_cancel(&self) {
		if let Some(task) = self.login.lock().unwrap().take() {
			task.abort();
		}
	}

	pub fn logout(&self) -> Result<Value, String> {
		self.login_cancel();
		self.store
			.as_ref()
			.ok_or("Command Code sign-in is not available")?
			.clear()?;
		self.reconnect();
		self.harness
			.emit(json!({"method":"account/updated","params":{"success":true}}));
		Ok(self.account())
	}
}
