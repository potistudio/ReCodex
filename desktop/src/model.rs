//! Model contract: the normalized streaming request and response, and the [`ModelClient`]
//! trait. The agent harness consumes this contract and model providers implement it; neither
//! depends on the other.
//!
//! The canonical request uses Chat Completions messages (`system`, `user`, `assistant` with
//! `tool_calls`, and `tool`).
use serde_json::{json, Value};
use std::{future::Future, pin::Pin};

pub struct ModelRequest {
	pub model: String,
	pub messages: Vec<Value>,
	pub tools: Vec<Value>,
	pub effort: Option<String>,
	/// Project root of the chat; some providers attach it to the request.
	pub cwd: String,
	/// Conversation identifier of the chat, used by providers that accept one. Providers that
	/// need a specific format, such as a UUID, ignore a value that does not match it.
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
	pub(crate) fn new(message: impl Into<String>, retryable: bool) -> Self {
		Self {
			message: message.into(),
			retryable,
			status: None,
			code: None,
		}
	}
}

pub struct ModelInfo {
	pub id: String,
	pub context_window: Option<u64>,
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
