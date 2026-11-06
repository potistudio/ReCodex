//! Model Gateway: a normalized streaming request and the wire adapters behind it.
//! Provider-specific formats stay inside adapters; the run driver only sees `ModelRequest`,
//! `Delta`, and `ModelResponse`.
//!
//! The canonical request uses Chat Completions messages (`system`, `user`, `assistant` with
//! `tool_calls`, and `tool`). Other adapters convert it through [`turns`] and
//! [`function_tools`].
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, time::Duration};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ModelRequest {
	pub model: String,
	pub messages: Vec<Value>,
	pub tools: Vec<Value>,
	pub effort: Option<String>,
	/// Project root of the chat; some providers attach it to the request.
	pub cwd: String,
	/// Chat ID, used as a conversation identifier by providers that accept one.
	pub session: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCallRequest {
	pub id: String,
	pub name: String,
	pub arguments: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
	pub input: u64,
	pub cached: u64,
	pub output: u64,
	pub reasoning: u64,
	pub total: u64,
}

impl Usage {
	pub fn to_value(self) -> Value {
		json!({
			"input": self.input,
			"cached": self.cached,
			"output": self.output,
			"reasoning": self.reasoning,
			"total": self.total
		})
	}

	pub fn from_value(value: &Value) -> Self {
		let number = |key: &str| value[key].as_u64().unwrap_or_default();
		Self {
			input: number("input"),
			cached: number("cached"),
			output: number("output"),
			reasoning: number("reasoning"),
			total: number("total"),
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct ModelResponse {
	pub text: String,
	pub reasoning: String,
	pub tool_calls: Vec<ToolCallRequest>,
	pub finish_reason: Option<String>,
	pub usage: Option<Usage>,
}

impl ModelResponse {
	/// Drop nameless calls and give every call an ID.
	pub(super) fn finish(mut self) -> Self {
		self.tool_calls.retain(|call| !call.name.is_empty());
		for (index, call) in self.tool_calls.iter_mut().enumerate() {
			if call.id.is_empty() {
				call.id = format!("call_{index}");
			}
			if call.arguments.trim().is_empty() {
				call.arguments = "{}".into();
			}
		}
		self
	}
}

pub enum Delta {
	Text(String),
	Reasoning(String),
}

#[derive(Debug)]
pub struct ModelError {
	pub message: String,
	/// True only when no output reached the caller, so a new attempt cannot duplicate it.
	pub retryable: bool,
	/// HTTP status of a rejected request.
	pub status: Option<u16>,
	/// Provider error code from the response body, such as `upgrade_required`.
	pub code: Option<String>,
}

impl ModelError {
	pub(super) fn new(message: impl Into<String>, retryable: bool) -> Self {
		Self {
			message: message.into(),
			retryable,
			status: None,
			code: None,
		}
	}
}

/// The wire protocol that serves a model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
	#[default]
	ChatCompletions,
	Responses,
	AnthropicMessages,
}

pub struct ModelInfo {
	pub id: String,
	pub context_window: Option<u64>,
	pub route: Route,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type OnDelta<'a> = &'a (dyn Fn(Delta) + Send + Sync);

pub trait ModelClient: Send + Sync {
	fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>>;
	fn stream<'a>(
		&'a self,
		request: &'a ModelRequest,
		on_delta: OnDelta<'a>,
	) -> BoxFuture<'a, Result<ModelResponse, ModelError>>;
}

pub(super) fn http_client() -> Result<reqwest::Client, String> {
	reqwest::Client::builder()
		.connect_timeout(Duration::from_secs(30))
		.build()
		.map_err(|e| e.to_string())
}

pub(super) fn error_message(body: &Value) -> String {
	body["error"]["message"]
		.as_str()
		.or_else(|| body["error"].as_str())
		.or_else(|| body["message"].as_str())
		.map(str::to_owned)
		.unwrap_or_else(|| body.to_string().chars().take(2000).collect())
}

/// Send a request and turn transport failures and non-2xx responses into `ModelError`s.
pub(super) async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, ModelError> {
	let response = request
		.send()
		.await
		.map_err(|e| ModelError::new(format!("Model request failed: {e}"), true))?;
	let status = response.status();
	if status.is_success() {
		return Ok(response);
	}
	let text = response.text().await.unwrap_or_default();
	let body = serde_json::from_str::<Value>(&text).ok();
	let detail = body
		.as_ref()
		.map(error_message)
		.unwrap_or_else(|| text.chars().take(2000).collect());
	let code = body.as_ref().and_then(|body| {
		body["error"]["code"]
			.as_str()
			.or_else(|| body["code"].as_str())
			.map(str::to_owned)
	});
	Err(ModelError {
		message: format!("Model request failed with {status}: {detail}"),
		retryable: status.as_u16() == 429 || status.is_server_error(),
		status: Some(status.as_u16()),
		code,
	})
}

/// Feed response chunks to `on_chunk` until it returns `Ok(true)` (done) or the body ends.
/// Returns whether `on_chunk` reported completion. Transport failures are retryable; the
/// caller withdraws that once output has been shown.
pub(super) async fn read_stream(
	response: reqwest::Response,
	mut on_chunk: impl FnMut(&[u8]) -> Result<bool, ModelError>,
) -> Result<bool, ModelError> {
	let mut stream = response.bytes_stream();
	loop {
		match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
			Err(_) => return Err(ModelError::new("The model stream stopped responding", true)),
			Ok(None) => return Ok(false),
			Ok(Some(Err(e))) => return Err(ModelError::new(format!("The model stream failed: {e}"), true)),
			Ok(Some(Ok(chunk))) => {
				if on_chunk(&chunk)? {
					return Ok(true);
				}
			}
		}
	}
}

/// Splits a byte stream into lines, tolerating `\r\n` and chunk boundaries inside lines.
#[derive(Default)]
pub(super) struct Lines {
	buffer: Vec<u8>,
}

impl Lines {
	pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
		self.buffer.extend_from_slice(bytes);
		let mut lines = Vec::new();
		while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
			let line: Vec<u8> = self.buffer.drain(..=end).collect();
			lines.push(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned());
		}
		lines
	}

	/// The unterminated last line, if any.
	pub fn rest(&mut self) -> Option<String> {
		let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buffer))
			.trim()
			.to_owned();
		(!rest.is_empty()).then_some(rest)
	}
}

pub(super) struct SseEvent {
	pub event: String,
	pub data: String,
}

/// Incremental Server-Sent Events decoder.
#[derive(Default)]
pub(super) struct SseDecoder {
	lines: Lines,
	event: String,
	data: Vec<String>,
}

impl SseDecoder {
	pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
		let mut events = Vec::new();
		for line in self.lines.push(bytes) {
			if line.is_empty() {
				if !self.data.is_empty() {
					events.push(SseEvent {
						event: std::mem::take(&mut self.event),
						data: self.data.join("\n"),
					});
					self.data.clear();
				}
				self.event.clear();
			} else if let Some(data) = line.strip_prefix("data:") {
				self.data.push(data.strip_prefix(' ').unwrap_or(data).to_owned());
			} else if let Some(event) = line.strip_prefix("event:") {
				self.event = event.trim().to_owned();
			}
		}
		events
	}
}

/// One entry of the canonical conversation.
#[derive(Debug, PartialEq)]
pub(super) enum Turn {
	User(String),
	Assistant {
		text: String,
		calls: Vec<ToolCallRequest>,
	},
	Tool {
		call_id: String,
		/// The calling tool's name, resolved from the assistant message that requested it.
		name: String,
		content: String,
	},
}

/// Split canonical messages into the system prompt and the remaining turns.
pub(super) fn turns(messages: &[Value]) -> (String, Vec<Turn>) {
	let mut system = Vec::new();
	let mut turns = Vec::new();
	let mut names = std::collections::HashMap::new();
	for message in messages {
		let text = message["content"].as_str().unwrap_or_default().to_owned();
		match message["role"].as_str().unwrap_or_default() {
			"system" | "developer" => system.push(text),
			"user" => turns.push(Turn::User(text)),
			"assistant" => {
				let calls: Vec<ToolCallRequest> = message["tool_calls"]
					.as_array()
					.map(Vec::as_slice)
					.unwrap_or_default()
					.iter()
					.map(|call| ToolCallRequest {
						id: call["id"].as_str().unwrap_or_default().to_owned(),
						name: call["function"]["name"].as_str().unwrap_or_default().to_owned(),
						arguments: call["function"]["arguments"].as_str().unwrap_or("{}").to_owned(),
					})
					.collect();
				for call in &calls {
					names.insert(call.id.clone(), call.name.clone());
				}
				turns.push(Turn::Assistant { text, calls });
			}
			"tool" => {
				let call_id = message["tool_call_id"].as_str().unwrap_or_default().to_owned();
				turns.push(Turn::Tool {
					name: names.get(&call_id).cloned().unwrap_or_default(),
					call_id,
					content: text,
				});
			}
			_ => {}
		}
	}
	(system.join("\n\n"), turns)
}

/// `(name, description, JSON Schema)` of each Chat Completions function tool.
pub(super) fn function_tools(tools: &[Value]) -> Vec<(String, String, Value)> {
	tools
		.iter()
		.filter_map(|tool| {
			let function = &tool["function"];
			Some((
				function["name"].as_str()?.to_owned(),
				function["description"].as_str().unwrap_or_default().to_owned(),
				function["parameters"].clone(),
			))
		})
		.collect()
}

/// Parse tool arguments as a JSON object; malformed arguments become an empty object.
pub(super) fn arguments_object(arguments: &str) -> Value {
	serde_json::from_str::<Value>(arguments)
		.ok()
		.filter(Value::is_object)
		.unwrap_or_else(|| json!({}))
}

/// OpenAI-compatible Chat Completions adapter.
pub struct OpenAiCompatible {
	client: reqwest::Client,
	base_url: String,
	api_key: Option<String>,
	models: Option<Vec<String>>,
}

impl OpenAiCompatible {
	pub fn new(base_url: String, api_key: Option<String>, models: Option<Vec<String>>) -> Result<Self, String> {
		Ok(Self {
			client: http_client()?,
			base_url: base_url.trim_end_matches('/').to_owned(),
			api_key,
			models,
		})
	}

	async fn list(&self) -> Result<Vec<ModelInfo>, String> {
		if let Some(models) = &self.models {
			return Ok(models
				.iter()
				.map(|id| ModelInfo {
					id: id.clone(),
					context_window: None,
					route: Route::ChatCompletions,
				})
				.collect());
		}
		let mut request = self.client.get(format!("{}/models", self.base_url));
		if let Some(key) = &self.api_key {
			request = request.bearer_auth(key);
		}
		list_models(request).await
	}
}

/// Read an OpenAI-style `GET /models` catalog. `supported_endpoints`, when present, selects
/// the route; Claude models are served only through Anthropic Messages.
pub(super) async fn list_models(request: reqwest::RequestBuilder) -> Result<Vec<ModelInfo>, String> {
	let response = request
		.timeout(Duration::from_secs(30))
		.send()
		.await
		.map_err(|e| format!("Model list request failed: {e}"))?;
	let status = response.status();
	let body: Value = response
		.json()
		.await
		.map_err(|e| format!("Model list response is not JSON: {e}"))?;
	if !status.is_success() {
		return Err(format!("Model list failed with {status}: {}", error_message(&body)));
	}
	let mut models: Vec<ModelInfo> = body["data"]
		.as_array()
		.map(Vec::as_slice)
		.unwrap_or_default()
		.iter()
		.filter_map(|model| {
			let id = model["id"].as_str()?.to_owned();
			let endpoints: Vec<&str> = model["supported_endpoints"]
				.as_array()
				.map(|endpoints| endpoints.iter().filter_map(Value::as_str).collect())
				.unwrap_or_default();
			let route = if id.starts_with("claude-")
				|| (!endpoints.is_empty() && endpoints.iter().all(|endpoint| endpoint.contains("messages")))
			{
				Route::AnthropicMessages
			} else if endpoints.iter().any(|endpoint| endpoint.ends_with("/responses")) {
				Route::Responses
			} else {
				Route::ChatCompletions
			};
			Some(ModelInfo {
				id,
				context_window: model["context_length"]
					.as_u64()
					.or_else(|| model["context_window"].as_u64()),
				route,
			})
		})
		.collect();
	models.sort_by(|a, b| a.id.cmp(&b.id));
	Ok(models)
}

/// Stream a Chat Completions request to `{base_url}/chat/completions`.
pub(super) async fn stream_chat(
	request: reqwest::RequestBuilder,
	model: &ModelRequest,
	on_delta: OnDelta<'_>,
) -> Result<ModelResponse, ModelError> {
	let mut body = json!({
		"model": model.model,
		"messages": model.messages,
		"stream": true,
		"stream_options": {"include_usage": true}
	});
	if !model.tools.is_empty() {
		body["tools"] = json!(model.tools);
	}
	if let Some(effort) = &model.effort {
		body["reasoning_effort"] = json!(effort);
	}
	let response = send(request.json(&body)).await?;
	let mut decoder = SseDecoder::default();
	let mut chat = ChatAccumulator::default();
	let mut produced = false;
	let done = read_stream(response, |chunk| {
		for event in decoder.push(chunk) {
			if event.data == "[DONE]" {
				return Ok(true);
			}
			let value: Value = serde_json::from_str(&event.data)
				.map_err(|e| ModelError::new(format!("Malformed model stream event: {e}"), false))?;
			if value.get("error").is_some_and(|e| !e.is_null()) {
				return Err(ModelError::new(
					format!("The model reported an error: {}", error_message(&value)),
					false,
				));
			}
			for delta in chat.apply(&value) {
				produced = true;
				on_delta(delta);
			}
		}
		Ok(false)
	})
	.await
	.map_err(|mut error| {
		error.retryable &= !produced;
		error
	})?;
	if done || chat.saw_event {
		Ok(chat.response.finish())
	} else {
		Err(ModelError::new("The model stream ended without a response", true))
	}
}

impl ModelClient for OpenAiCompatible {
	fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>> {
		Box::pin(self.list())
	}

	fn stream<'a>(
		&'a self,
		request: &'a ModelRequest,
		on_delta: OnDelta<'a>,
	) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
		let mut http = self.client.post(format!("{}/chat/completions", self.base_url));
		if let Some(key) = &self.api_key {
			http = http.bearer_auth(key);
		}
		Box::pin(stream_chat(http, request, on_delta))
	}
}

/// Chat Completions chunk accumulator.
#[derive(Default)]
struct ChatAccumulator {
	response: ModelResponse,
	saw_event: bool,
}

impl ChatAccumulator {
	fn apply(&mut self, chunk: &Value) -> Vec<Delta> {
		self.saw_event = true;
		let mut deltas = Vec::new();
		if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
			let number = |value: &Value| value.as_u64().unwrap_or_default();
			let input = number(&usage["prompt_tokens"]);
			let output = number(&usage["completion_tokens"]);
			self.response.usage = Some(Usage {
				input,
				cached: number(&usage["prompt_tokens_details"]["cached_tokens"]),
				output,
				reasoning: number(&usage["completion_tokens_details"]["reasoning_tokens"]),
				total: usage["total_tokens"].as_u64().unwrap_or(input + output),
			});
		}
		let Some(choice) = chunk["choices"].get(0) else {
			return deltas;
		};
		let delta = &choice["delta"];
		if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
			self.response.text.push_str(text);
			deltas.push(Delta::Text(text.to_owned()));
		}
		if let Some(text) = delta["reasoning_content"]
			.as_str()
			.or_else(|| delta["reasoning"].as_str())
			.filter(|text| !text.is_empty())
		{
			self.response.reasoning.push_str(text);
			deltas.push(Delta::Reasoning(text.to_owned()));
		}
		for call in delta["tool_calls"].as_array().map(Vec::as_slice).unwrap_or_default() {
			let index = call["index"]
				.as_u64()
				.map(|index| index as usize)
				.unwrap_or(self.response.tool_calls.len());
			while self.response.tool_calls.len() <= index {
				self.response.tool_calls.push(ToolCallRequest::default());
			}
			let entry = &mut self.response.tool_calls[index];
			if let Some(id) = call["id"].as_str() {
				entry.id = id.to_owned();
			}
			if let Some(name) = call["function"]["name"].as_str() {
				entry.name.push_str(name);
			}
			if let Some(arguments) = call["function"]["arguments"].as_str() {
				entry.arguments.push_str(arguments);
			}
		}
		if let Some(reason) = choice["finish_reason"].as_str() {
			self.response.finish_reason = Some(reason.to_owned());
		}
		deltas
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stream_chunks_accumulate_text_reasoning_tools_and_usage() {
		let mut decoder = SseDecoder::default();
		let mut chat = ChatAccumulator::default();
		let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\",\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4,\"total_tokens\":14}}\n\n",
            "data: [DONE]\n\n"
        );
		let mut deltas = 0;
		let mut done = false;
		// Split mid-line to exercise buffering across network chunks.
		for piece in stream.as_bytes().chunks(7) {
			for event in decoder.push(piece) {
				if event.data == "[DONE]" {
					done = true;
				} else {
					deltas += chat.apply(&serde_json::from_str(&event.data).unwrap()).len();
				}
			}
		}
		assert!(done);
		assert_eq!(deltas, 3);
		let response = chat.response.finish();
		assert_eq!(response.text, "Hello");
		assert_eq!(response.reasoning, "think");
		assert_eq!(
			response.tool_calls,
			vec![ToolCallRequest {
				id: "c1".into(),
				name: "read_file".into(),
				arguments: "{\"path\":\"a\"}".into()
			}]
		);
		assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
		assert_eq!(response.usage.unwrap().total, 14);
	}

	#[test]
	fn canonical_messages_split_into_turns_with_tool_names() {
		let (system, turns) = turns(&[
			json!({"role":"system","content":"rules"}),
			json!({"role":"user","content":"hi"}),
			json!({"role":"assistant","content":"","tool_calls":[
				{"id":"c","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}
			]}),
			json!({"role":"tool","tool_call_id":"c","content":"data"}),
		]);
		assert_eq!(system, "rules");
		assert_eq!(turns.len(), 3);
		assert_eq!(
			turns[2],
			Turn::Tool {
				call_id: "c".into(),
				name: "read_file".into(),
				content: "data".into()
			}
		);
		assert_eq!(arguments_object("not json"), json!({}));
	}

	#[test]
	fn sse_events_keep_their_names() {
		let mut decoder = SseDecoder::default();
		let events = decoder.push(b"event: message_start\ndata: {\"a\":1}\n\n: comment\ndata: x\n\n");
		assert_eq!(events.len(), 2);
		assert_eq!(events[0].event, "message_start");
		assert_eq!(events[1].event, "");
		assert_eq!(events[1].data, "x");
	}
}
