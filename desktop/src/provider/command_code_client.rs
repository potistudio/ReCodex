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
	gateway::{http_client, list_models, stream_chat, CatalogModel, Route},
	generate_api, messages_api, responses_api,
};
use crate::model::{BoxFuture, ModelClient, ModelError, ModelInfo, ModelRequest, ModelResponse, OnDelta};
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
				.map(|id| CatalogModel {
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
		Ok(models.into_iter().map(ModelInfo::from).collect())
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::Delta;
	use serde_json::{json, Value};

	fn command_code_request(model: &str) -> ModelRequest {
		ModelRequest {
			model: model.into(),
			messages: vec![
				json!({"role":"system","content":"rules"}),
				json!({"role":"user","content":"hello"}),
			],
			tools: crate::test_support::tool_schemas(),
			effort: None,
			cwd: "/work/project".into(),
			session: uuid::Uuid::nil().to_string(),
		}
	}

	#[tokio::test]
	async fn command_code_routes_models_and_falls_back_to_generate() {
		let catalog = json!({"data":[
        {"id":"claude-sonnet-4-6","name":"Claude","context_length":200000,"supported_endpoints":["/messages"]},
        {"id":"gpt-5.6","name":"GPT","context_length":400000,"supported_endpoints":["/chat/completions","/responses"]},
        {"id":"deepseek/deepseek-v4-flash","name":"DeepSeek","context_length":1000000,"supported_endpoints":["/chat/completions"]}
    ]})
    .to_string();
		let (api, seen) = crate::test_support::mock_http(move |_, path| match path {
        "/provider/v1/models" => ("200 OK", "application/json", catalog.clone()),
        "/provider/v1/messages" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"from claude\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            )
            .into(),
        ),
        "/provider/v1/responses" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"from responses\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
            )
            .into(),
        ),
        "/provider/v1/chat/completions" => (
            "403 Forbidden",
            "application/json",
            r#"{"error":{"code":"upgrade_required","message":"Upgrade to use the Provider API"}}"#.into(),
        ),
        "/alpha/generate" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "data: {\"type\":\"text-delta\",\"text\":\"from generate\"}\n\n",
                "data: {\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":5,\"outputTokens\":2}}\n\n"
            )
            .into(),
        ),
        _ => ("404 Not Found", "application/json", "{}".into()),
    })
    .await;
		let client = CommandCodeClient::new(&api, "cc_key".into(), None).unwrap();
		let models = client.models().await.unwrap();
		assert_eq!(models.len(), 3);
		let text = |response: ModelResponse| response.text;

		let claude = client
			.stream(&command_code_request("claude-sonnet-4-6"), &|_| {})
			.await
			.unwrap();
		assert_eq!(text(claude), "from claude");
		let responses = client.stream(&command_code_request("gpt-5.6"), &|_| {}).await.unwrap();
		assert_eq!(text(responses), "from responses");
		assert!(!client.uses_generate());

		let streamed = Mutex::new(String::new());
		let fallback = client
			.stream(&command_code_request("deepseek/deepseek-v4-flash"), &|delta| {
				if let Delta::Text(text) = delta {
					streamed.lock().unwrap().push_str(&text);
				}
			})
			.await
			.unwrap();
		assert_eq!(fallback.usage.unwrap().total, 7);
		assert_eq!(text(fallback), "from generate");
		assert_eq!(*streamed.lock().unwrap(), "from generate");
		assert!(client.uses_generate());
		// Later requests go straight to generate, whatever the model's route.
		let again = client
			.stream(&command_code_request("claude-sonnet-4-6"), &|_| {})
			.await
			.unwrap();
		assert_eq!(text(again), "from generate");

		let seen = seen.lock().unwrap().clone();
		let paths: Vec<_> = seen.iter().map(|r| r.path.as_str()).collect();
		assert_eq!(
			paths,
			[
				"/provider/v1/models",
				"/provider/v1/messages",
				"/provider/v1/responses",
				"/provider/v1/chat/completions",
				"/alpha/generate",
				"/alpha/generate"
			]
		);
		assert!(seen.iter().all(|r| r.headers.contains("authorization: bearer cc_key")));
		assert_eq!(seen[0].method, "GET");
		assert!(seen[1..].iter().all(|r| r.method == "POST"));
		assert!(seen[1].headers.contains("anthropic-version: 2023-06-01"));
		let messages: Value = serde_json::from_str(&seen[1].body).unwrap();
		assert_eq!(messages["system"], "rules");
		assert_eq!(messages["tools"].as_array().unwrap().len(), 5);
		let responses: Value = serde_json::from_str(&seen[2].body).unwrap();
		assert_eq!(responses["instructions"], "rules");
		assert_eq!(responses["store"], false);
		let generate: Value = serde_json::from_str(&seen[4].body).unwrap();
		assert_eq!(generate["params"]["system"], "rules");
		assert_eq!(generate["params"]["messages"][0]["content"], "hello");
		assert_eq!(generate["config"]["workingDir"], "/work/project");
		assert_eq!(generate["threadId"], uuid::Uuid::nil().to_string());
		assert!(seen[4].headers.contains("x-project-slug: work-project"));
		assert!(seen[4].headers.contains("x-command-code-version:"));
	}
}
