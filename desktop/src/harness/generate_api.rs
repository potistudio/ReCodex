//! Command Code's CLI generation endpoint (`POST {api}/alpha/generate`).
//!
//! This is the endpoint the `cmd` CLI itself uses; plans without Provider API access (Go)
//! can still generate through it. It is not publicly documented. The request and stream
//! formats follow the community pi-commandcode-provider (MIT): AI SDK model messages in, AI
//! SDK stream parts out. Treat it as best-effort compatibility that may break.
use super::model::{
	arguments_object, error_message, function_tools, read_stream, send, turns, Delta, Lines, ModelError, ModelRequest,
	ModelResponse, OnDelta, ToolCallRequest, Turn, Usage,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const PATH: &str = "/alpha/generate";
/// CLI version announced to the endpoint; matches the CLI whose format this follows.
pub const CLI_VERSION: &str = "1.88.2";
const MAX_TOKENS: u64 = 64_000;
/// The endpoint rejects tool-call IDs longer than this.
const ID_LIMIT: usize = 64;

/// Shorten an over-long tool-call ID deterministically, so calls and results still pair.
fn wire_id(id: &str) -> String {
	if id.len() <= ID_LIMIT {
		return id.to_owned();
	}
	let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
	let head: String = id
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
				c
			} else {
				'_'
			}
		})
		.take(ID_LIMIT - 9)
		.collect();
	format!("{head}_{}", &hash[..8])
}

/// A project slug like the CLI's: lowercase, alphanumeric runs joined by '-'.
pub(super) fn project_slug(path: &str) -> String {
	let lower = path.to_lowercase();
	let without_drive = if lower.len() > 1 && lower.as_bytes()[1] == b':' {
		&lower[2..]
	} else {
		&lower
	};
	let slug = without_drive
		.split(|c: char| !c.is_ascii_alphanumeric())
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>()
		.join("-");
	if slug.is_empty() {
		"project".into()
	} else {
		slug
	}
}

pub(super) fn body(request: &ModelRequest, date: &str) -> Value {
	let (system, turns) = turns(&request.messages);
	let mut messages = Vec::new();
	for turn in turns {
		match turn {
			Turn::User(text) => messages.push(json!({"role":"user","content":text})),
			Turn::Assistant { text, calls } => {
				let mut parts = Vec::new();
				if !text.is_empty() {
					parts.push(json!({"type":"text","text":text}));
				}
				for call in calls {
					parts.push(json!({
						"type":"tool-call",
						"toolCallId":wire_id(&call.id),
						"toolName":call.name,
						"input":arguments_object(&call.arguments)
					}));
				}
				if !parts.is_empty() {
					messages.push(json!({"role":"assistant","content":parts}));
				}
			}
			Turn::Tool { call_id, name, content } => messages.push(json!({
				"role":"tool",
				"content":[{
					"type":"tool-result",
					"toolCallId":wire_id(&call_id),
					"toolName":name,
					"output":{"type":"text","value":content}
				}]
			})),
		}
	}
	let tools: Vec<Value> = function_tools(&request.tools)
		.into_iter()
		.map(
			|(name, description, schema)| json!({"type":"function","name":name,"description":description,"input_schema":schema}),
		)
		.collect();
	let mut params = json!({
		"model": request.model,
		"messages": messages,
		"tools": tools,
		"system": system,
		"max_tokens": MAX_TOKENS,
		"stream": true
	});
	if let Some(effort) = &request.effort {
		params["reasoning_effort"] = json!(effort);
	}
	let thread = request
		.session
		.strip_prefix(super::THREAD_PREFIX)
		.filter(|id| uuid::Uuid::parse_str(id).is_ok())
		.map(str::to_owned)
		.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
	json!({
		"config": {
			"workingDir": request.cwd,
			"date": date,
			"environment": format!("{}-{}, ReCodex", std::env::consts::OS, std::env::consts::ARCH),
			"structure": [],
			"isGitRepo": false,
			"currentBranch": "",
			"mainBranch": "",
			"gitStatus": "",
			"recentCommits": []
		},
		"memory": Value::Null,
		"taste": Value::Null,
		"skills": Value::Null,
		"params": params,
		"threadId": thread
	})
}

#[derive(Default)]
struct Accumulator {
	response: ModelResponse,
	/// Streaming tool input by call ID → position in `response.tool_calls`.
	calls: HashMap<String, usize>,
	finished: bool,
}

impl Accumulator {
	fn call(&mut self, id: &str) -> usize {
		if let Some(position) = self.calls.get(id) {
			return *position;
		}
		self.calls.insert(id.to_owned(), self.response.tool_calls.len());
		self.response.tool_calls.push(ToolCallRequest {
			id: id.to_owned(),
			..ToolCallRequest::default()
		});
		self.response.tool_calls.len() - 1
	}

	fn apply(&mut self, event: &Value) -> Result<Vec<Delta>, ModelError> {
		let mut deltas = Vec::new();
		let text = |key: &str| event[key].as_str().unwrap_or_default().to_owned();
		match event["type"].as_str().unwrap_or_default() {
			"text-delta" => {
				let delta = text("text");
				if !delta.is_empty() {
					self.response.text.push_str(&delta);
					deltas.push(Delta::Text(delta));
				}
			}
			"reasoning-delta" => {
				let delta = text("text");
				if !delta.is_empty() {
					self.response.reasoning.push_str(&delta);
					deltas.push(Delta::Reasoning(delta));
				}
			}
			"tool-input-start" => {
				let position = self.call(&text("id"));
				self.response.tool_calls[position].name = text("toolName");
			}
			"tool-input-delta" => {
				let position = self.call(&text("id"));
				self.response.tool_calls[position].arguments.push_str(&text("delta"));
			}
			"tool-call" => {
				let position = self.call(&text("toolCallId"));
				let call = &mut self.response.tool_calls[position];
				if let Some(name) = event["toolName"].as_str() {
					call.name = name.to_owned();
				}
				let input = event
					.get("input")
					.or_else(|| event.get("args"))
					.or_else(|| event.get("arguments"));
				match input {
					Some(Value::String(arguments)) => call.arguments = arguments.clone(),
					Some(value) if value.is_object() => call.arguments = value.to_string(),
					_ => {}
				}
			}
			"finish" => {
				let raw = text("rawFinishReason").to_lowercase();
				if ["network", "connection", "upstream"]
					.iter()
					.any(|prefix| raw.starts_with(prefix) && raw.ends_with("error"))
				{
					return Err(ModelError::new(
						format!("Command Code finished with \"{raw}\": the upstream connection failed"),
						false,
					));
				}
				let usage = &event["totalUsage"];
				if usage.is_object() {
					let number = |value: &Value| value.as_u64().unwrap_or_default();
					let details = &usage["inputTokenDetails"];
					let input = number(&usage["inputTokens"]);
					let output = number(&usage["outputTokens"]);
					self.response.usage = Some(Usage {
						input,
						cached: number(&details["cacheReadTokens"]),
						output,
						reasoning: number(&usage["reasoningTokens"]),
						total: input + output,
					});
				}
				self.response.finish_reason = Some(match event["finishReason"].as_str() {
					Some("tool-calls") => "tool_calls".into(),
					Some(reason) => reason.to_owned(),
					None => "stop".into(),
				});
				self.finished = true;
			}
			"abort" => return Err(ModelError::new("Command Code aborted the request", false)),
			"error" => {
				let message = match &event["error"] {
					Value::String(message) => message.clone(),
					Value::Null => error_message(event),
					error => error_message(&json!({ "error": error })),
				};
				return Err(ModelError::new(
					format!("The model reported an error: {message}"),
					false,
				));
			}
			_ => {}
		}
		Ok(deltas)
	}
}

/// Lines are SSE `data:` fields or bare JSON objects.
fn parse_line(line: &str) -> Option<Value> {
	let line = line.trim();
	if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
		return None;
	}
	let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
	if data == "[DONE]" {
		return None;
	}
	serde_json::from_str(data).ok()
}

/// Stream one generation. `request` must carry the URL and authentication.
pub(super) async fn stream(
	request: reqwest::RequestBuilder,
	model: &ModelRequest,
	on_delta: OnDelta<'_>,
) -> Result<ModelResponse, ModelError> {
	let date = super::command_code::timestamp_now()[..10].to_owned();
	let request = request
		.header("x-command-code-version", CLI_VERSION)
		.header("x-cli-environment", "production")
		.header("x-project-slug", project_slug(&model.cwd))
		.header("x-taste-learning", "false")
		.header("User-Agent", "cli")
		.json(&body(model, &date));
	let response = send(request).await?;
	let mut lines = Lines::default();
	let mut accumulator = Accumulator::default();
	let mut produced = false;
	let mut handle = |line: &str, produced: &mut bool| -> Result<bool, ModelError> {
		if let Some(event) = parse_line(line) {
			for delta in accumulator.apply(&event)? {
				*produced = true;
				on_delta(delta);
			}
		}
		Ok(accumulator.finished)
	};
	let done = read_stream(response, |chunk| {
		for line in lines.push(chunk) {
			if handle(&line, &mut produced)? {
				return Ok(true);
			}
		}
		Ok(false)
	})
	.await
	.map_err(|mut error| {
		error.retryable &= !produced;
		error
	})?;
	if !done {
		if let Some(rest) = lines.rest() {
			handle(&rest, &mut produced)?;
		}
	}
	if !accumulator.finished {
		return Err(ModelError::new(
			"The Command Code stream ended before completion",
			!produced,
		));
	}
	Ok(accumulator.response.finish())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn requests_use_ai_sdk_messages_and_bounded_ids() {
		let long = "call_".to_owned() + &"a".repeat(80);
		let body = body(
			&ModelRequest {
				model: "deepseek/deepseek-v4-flash".into(),
				messages: vec![
					json!({"role":"system","content":"rules"}),
					json!({"role":"user","content":"hi"}),
					json!({"role":"assistant","content":"","tool_calls":[
						{"id":long,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}
					]}),
					json!({"role":"tool","tool_call_id":long,"content":"data"}),
				],
				tools: vec![
					json!({"type":"function","function":{"name":"read_file","description":"Read","parameters":{"type":"object"}}}),
				],
				effort: Some("high".into()),
				cwd: "/Users/me/My Project".into(),
				session: format!("rx-{}", uuid::Uuid::nil()),
			},
			"2026-11-04",
		);
		let params = &body["params"];
		assert_eq!(params["system"], "rules");
		assert_eq!(params["reasoning_effort"], "high");
		assert_eq!(params["tools"][0]["input_schema"], json!({"type":"object"}));
		let call = &params["messages"][1]["content"][0];
		let result = &params["messages"][2]["content"][0];
		assert_eq!(call["input"], json!({"path":"a"}));
		assert!(call["toolCallId"].as_str().unwrap().len() <= ID_LIMIT);
		assert_eq!(call["toolCallId"], result["toolCallId"]);
		assert_eq!(result["toolName"], "read_file");
		assert_eq!(body["threadId"], uuid::Uuid::nil().to_string());
		assert_eq!(body["config"]["workingDir"], "/Users/me/My Project");
		assert_eq!(project_slug("C:\\Users\\Me\\My Project"), "users-me-my-project");
	}

	#[test]
	fn stream_parts_build_text_calls_and_usage() {
		let mut accumulator = Accumulator::default();
		for line in [
            "data: {\"type\":\"reasoning-delta\",\"text\":\"hmm\"}",
            "{\"type\":\"text-delta\",\"text\":\"Hi\"}",
            "data: {\"type\":\"tool-input-start\",\"id\":\"t1\",\"toolName\":\"read_file\"}",
            "data: {\"type\":\"tool-input-delta\",\"id\":\"t1\",\"delta\":\"{\\\"pa\"}",
            "data: {\"type\":\"tool-call\",\"toolCallId\":\"t1\",\"toolName\":\"read_file\",\"input\":{\"path\":\"a\"}}",
            "event: ignored",
            "data: {\"type\":\"finish\",\"finishReason\":\"tool-calls\",\"totalUsage\":{\"inputTokens\":9,\"outputTokens\":2,\"inputTokenDetails\":{\"cacheReadTokens\":4}}}",
        ] {
            if let Some(event) = parse_line(line) {
                accumulator.apply(&event).unwrap();
            }
        }
		assert!(accumulator.finished);
		let response = accumulator.response.finish();
		assert_eq!(response.text, "Hi");
		assert_eq!(response.reasoning, "hmm");
		assert_eq!(response.tool_calls[0].arguments, "{\"path\":\"a\"}");
		assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
		assert_eq!(response.usage.unwrap().cached, 4);
		assert!(Accumulator::default()
			.apply(&json!({"type":"finish","rawFinishReason":"upstream_error"}))
			.is_err());
		assert!(Accumulator::default()
			.apply(&json!({"type":"error","error":{"message":"quota"}}))
			.is_err());
	}
}
