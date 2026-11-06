//! Command Code model client: routes each model to the Provider API wire that serves it and
//! falls back to the CLI generation endpoint for plans without Provider API access.
//!
//! - Claude models → Anthropic Messages (`/provider/v1/messages`)
//! - Models advertising `/responses` → OpenAI Responses (`/provider/v1/responses`)
//! - Other models → Chat Completions (`/provider/v1/chat/completions`)
//!
//! When the Provider API answers `403` with `upgrade_required`, this key is switched to
//! `/alpha/generate` for every later request.
use super::{
	command_code::PROVIDER_PATH,
	generate_api, messages_api,
	model::{
		http_client, list_models, stream_chat, BoxFuture, ModelClient, ModelError, ModelInfo, ModelRequest,
		ModelResponse, OnDelta, Route,
	},
	responses_api,
};
use std::{
	collections::HashMap,
	sync::{
		atomic::{AtomicBool, Ordering},
		Mutex,
	},
};

pub struct CommandCodeClient {
	client: reqwest::Client,
	/// `https://api.commandcode.ai`; Provider API routes live under [`PROVIDER_PATH`].
	api_url: String,
	api_key: String,
	models: Option<Vec<String>>,
	routes: Mutex<HashMap<String, Route>>,
	/// Set once the Provider API reported that this key's plan lacks access.
	generate: AtomicBool,
}

impl CommandCodeClient {
	pub fn new(api_url: &str, api_key: String, models: Option<Vec<String>>) -> Result<Self, String> {
		Ok(Self {
			client: http_client()?,
			api_url: api_url.trim_end_matches('/').to_owned(),
			api_key,
			models,
			routes: Mutex::new(HashMap::new()),
			generate: AtomicBool::new(false),
		})
	}

	pub fn uses_generate(&self) -> bool {
		self.generate.load(Ordering::SeqCst)
	}

	fn provider(&self, path: &str) -> reqwest::RequestBuilder {
		self.client
			.post(format!("{}{PROVIDER_PATH}{path}", self.api_url))
			.bearer_auth(&self.api_key)
	}

	fn route(&self, model: &str) -> Route {
		self.routes
			.lock()
			.unwrap()
			.get(model)
			.copied()
			.unwrap_or(if model.starts_with("claude-") {
				Route::AnthropicMessages
			} else {
				Route::ChatCompletions
			})
	}

	async fn list(&self) -> Result<Vec<ModelInfo>, String> {
		let models = match &self.models {
			Some(ids) => ids
				.iter()
				.map(|id| ModelInfo {
					id: id.clone(),
					context_window: None,
					route: if id.starts_with("claude-") {
						Route::AnthropicMessages
					} else {
						Route::ChatCompletions
					},
				})
				.collect(),
			None => {
				let url = format!("{}{PROVIDER_PATH}/models", self.api_url);
				match list_models(self.client.get(&url).bearer_auth(&self.api_key)).await {
					Ok(models) => models,
					// The catalog is public; plans without Provider API access may be refused.
					Err(_) => list_models(self.client.get(&url)).await?,
				}
			}
		};
		*self.routes.lock().unwrap() = models.iter().map(|model| (model.id.clone(), model.route)).collect();
		Ok(models)
	}

	async fn generate_stream(
		&self,
		request: &ModelRequest,
		on_delta: OnDelta<'_>,
	) -> Result<ModelResponse, ModelError> {
		let http = self
			.client
			.post(format!("{}{}", self.api_url, generate_api::PATH))
			.bearer_auth(&self.api_key);
		generate_api::stream(http, request, on_delta).await
	}

	async fn stream_routed(&self, request: &ModelRequest, on_delta: OnDelta<'_>) -> Result<ModelResponse, ModelError> {
		if self.uses_generate() {
			return self.generate_stream(request, on_delta).await;
		}
		let result = match self.route(&request.model) {
			Route::AnthropicMessages => messages_api::stream(self.provider("/messages"), request, on_delta).await,
			Route::Responses => responses_api::stream(self.provider("/responses"), request, on_delta).await,
			Route::ChatCompletions => stream_chat(self.provider("/chat/completions"), request, on_delta).await,
		};
		match result {
			Err(error) if error.status == Some(403) && error.code.as_deref() == Some("upgrade_required") => {
				// Rejected before any output, so the fallback cannot duplicate text.
				self.generate.store(true, Ordering::SeqCst);
				self.generate_stream(request, on_delta).await
			}
			result => result,
		}
	}
}

impl ModelClient for CommandCodeClient {
	fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>> {
		Box::pin(self.list())
	}

	fn stream<'a>(
		&'a self,
		request: &'a ModelRequest,
		on_delta: OnDelta<'a>,
	) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
		Box::pin(self.stream_routed(request, on_delta))
	}
}
