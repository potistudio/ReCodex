//! Anthropic Messages adapter (`POST {base}/messages`).
//!
//! Extended thinking is not requested: continuing a tool loop with thinking enabled requires
//! replaying signed thinking blocks, which the ledger does not keep.
use super::gateway::{arguments_object, error_message, function_tools, read_stream, send, turns, SseDecoder, Turn};
use crate::model::{Delta, ModelError, ModelRequest, ModelResponse, OnDelta, ToolCallRequest, Usage};
use serde_json::{json, Value};

pub const VERSION: &str = "2023-06-01";
const MAX_TOKENS: u64 = 32_000;

/// Convert canonical messages to Messages API `system` and `messages`. Consecutive turns of
/// the same role are merged, since the API requires alternating roles.
pub(super) fn body(request: &ModelRequest) -> Value {
	let (system, turns) = turns(&request.messages);
	let mut messages: Vec<Value> = Vec::new();
	let mut push = |role: &str, blocks: Vec<Value>| {
		if blocks.is_empty() {
			return;
		}
		match messages.last_mut() {
			Some(last) if last["role"] == role => {
				last["content"].as_array_mut().unwrap().extend(blocks);
			}
			_ => messages.push(json!({"role":role,"content":blocks})),
		}
	};
	for turn in turns {
		match turn {
			Turn::User(text) => push("user", vec![json!({"type":"text","text":text})]),
			Turn::Assistant { text, calls } => {
				let mut blocks = Vec::new();
				if !text.is_empty() {
					blocks.push(json!({"type":"text","text":text}));
				}
				for call in calls {
					blocks.push(json!({
						"type":"tool_use",
						"id":call.id,
						"name":call.name,
						"input":arguments_object(&call.arguments)
					}));
				}
				push("assistant", blocks);
			}
			Turn::Tool { call_id, content, .. } => push(
				"user",
				vec![json!({"type":"tool_result","tool_use_id":call_id,"content":content})],
			),
		}
	}
	let mut body = json!({
		"model": request.model,
		"max_tokens": MAX_TOKENS,
		"messages": messages,
		"stream": true
	});
	if !system.is_empty() {
		body["system"] = json!(system);
	}
	let tools: Vec<Value> = function_tools(&request.tools)
		.into_iter()
		.map(|(name, description, schema)| json!({"name":name,"description":description,"input_schema":schema}))
		.collect();
	if !tools.is_empty() {
		body["tools"] = json!(tools);
	}
	body
}

#[derive(Default)]
struct Accumulator {
	response: ModelResponse,
	/// Content block index → position in `response.tool_calls`.
	tools: std::collections::HashMap<u64, usize>,
	usage: Usage,
	finished: bool,
}

impl Accumulator {
	fn apply(&mut self, event: &Value) -> Result<Vec<Delta>, ModelError> {
		let mut deltas = Vec::new();
		let index = event["index"].as_u64().unwrap_or_default();
		match event["type"].as_str().unwrap_or_default() {
			"message_start" => self.usage_from(&event["message"]["usage"]),
			"content_block_start" => {
				let block = &event["content_block"];
				if block["type"] == "tool_use" {
					self.tools.insert(index, self.response.tool_calls.len());
					self.response.tool_calls.push(ToolCallRequest {
						id: block["id"].as_str().unwrap_or_default().to_owned(),
						name: block["name"].as_str().unwrap_or_default().to_owned(),
						arguments: String::new(),
					});
				}
			}
			"content_block_delta" => {
				let delta = &event["delta"];
				match delta["type"].as_str().unwrap_or_default() {
					"text_delta" => {
						let text = delta["text"].as_str().unwrap_or_default();
						if !text.is_empty() {
							self.response.text.push_str(text);
							deltas.push(Delta::Text(text.to_owned()));
						}
					}
					"thinking_delta" => {
						let text = delta["thinking"].as_str().unwrap_or_default();
						if !text.is_empty() {
							self.response.reasoning.push_str(text);
							deltas.push(Delta::Reasoning(text.to_owned()));
						}
					}
					"input_json_delta" => {
						if let Some(position) = self.tools.get(&index) {
							self.response.tool_calls[*position]
								.arguments
								.push_str(delta["partial_json"].as_str().unwrap_or_default());
						}
					}
					_ => {}
				}
			}
			"message_delta" => {
				if let Some(reason) = event["delta"]["stop_reason"].as_str() {
					self.response.finish_reason = Some(reason.to_owned());
				}
				self.usage_from(&event["usage"]);
			}
			"message_stop" => self.finished = true,
			"error" => {
				return Err(ModelError::new(
					format!("The model reported an error: {}", error_message(event)),
					false,
				))
			}
			_ => {}
		}
		Ok(deltas)
	}

	fn usage_from(&mut self, usage: &Value) {
		if !usage.is_object() {
			return;
		}
		let number = |key: &str| usage[key].as_u64();
		// Input counts arrive in message_start; output totals in message_delta.
		if let Some(input) = number("input_tokens") {
			self.usage.input = input
				+ number("cache_read_input_tokens").unwrap_or_default()
				+ number("cache_creation_input_tokens").unwrap_or_default();
			self.usage.cached = number("cache_read_input_tokens").unwrap_or_default();
		}
		if let Some(output) = number("output_tokens") {
			self.usage.output = output;
		}
		self.usage.total = self.usage.input + self.usage.output;
		self.response.usage = Some(self.usage);
	}
}

/// Stream one Messages request. `request` must carry the URL and authentication.
pub(super) async fn stream(
	request: reqwest::RequestBuilder,
	model: &ModelRequest,
	on_delta: OnDelta<'_>,
) -> Result<ModelResponse, ModelError> {
	let response = send(request.header("anthropic-version", VERSION).json(&body(model))).await?;
	let mut decoder = SseDecoder::default();
	let mut accumulator = Accumulator::default();
	let mut produced = false;
	read_stream(response, |chunk| {
		for event in decoder.push(chunk) {
			let value: Value = serde_json::from_str(&event.data)
				.map_err(|e| ModelError::new(format!("Malformed model stream event: {e}"), false))?;
			for delta in accumulator.apply(&value)? {
				produced = true;
				on_delta(delta);
			}
			if accumulator.finished {
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
	if !accumulator.finished {
		return Err(ModelError::new(
			"The model stream ended before the message was complete",
			!produced,
		));
	}
	Ok(accumulator.response.finish())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request(messages: Vec<Value>) -> ModelRequest {
		ModelRequest {
			model: "claude-sonnet-4-6".into(),
			messages,
			tools: vec![json!({"type":"function","function":{
				"name":"read_file","description":"Read","parameters":{"type":"object"}
			}})],
			effort: None,
			cwd: "/p".into(),
			session: "rx-1".into(),
		}
	}

	#[test]
	fn tool_results_and_a_following_prompt_share_one_user_message() {
		let body = body(&request(vec![
			json!({"role":"system","content":"rules"}),
			json!({"role":"user","content":"hi"}),
			json!({"role":"assistant","content":"Looking","tool_calls":[
				{"id":"a","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}},
				{"id":"b","type":"function","function":{"name":"read_file","arguments":""}}
			]}),
			json!({"role":"tool","tool_call_id":"a","content":"one"}),
			json!({"role":"tool","tool_call_id":"b","content":"two"}),
			json!({"role":"user","content":"continue"}),
		]));
		assert_eq!(body["system"], "rules");
		let messages = body["messages"].as_array().unwrap();
		assert_eq!(messages.len(), 3);
		assert_eq!(messages[1]["content"][1]["input"], json!({"path":"x"}));
		assert_eq!(messages[1]["content"][2]["input"], json!({}));
		let last = messages[2]["content"].as_array().unwrap();
		assert_eq!(last.len(), 3);
		assert_eq!(last[1]["tool_use_id"], "b");
		assert_eq!(last[2]["text"], "continue");
		assert_eq!(body["tools"][0]["input_schema"], json!({"type":"object"}));
	}

	#[test]
	fn stream_events_build_text_tools_and_usage() {
		let mut accumulator = Accumulator::default();
		for event in [
			json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5}}}),
			json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
			json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}),
			json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"read_file","input":{}}}),
			json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
			json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}),
			json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
			json!({"type":"message_stop"}),
		] {
			accumulator.apply(&event).unwrap();
		}
		assert!(accumulator.finished);
		let response = accumulator.response.finish();
		assert_eq!(response.text, "Hi");
		assert_eq!(response.tool_calls[0].arguments, "{\"path\":\"a\"}");
		assert_eq!(response.finish_reason.as_deref(), Some("tool_use"));
		let usage = response.usage.unwrap();
		assert_eq!((usage.input, usage.cached, usage.output, usage.total), (15, 5, 7, 22));
		assert!(Accumulator::default()
			.apply(&json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}))
			.is_err());
	}
}
