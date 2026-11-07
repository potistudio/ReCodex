//! OpenAI Responses adapter (`POST {base}/responses`).
//!
//! Requests are stateless (`store: false`); reasoning items are not replayed between steps.
use super::gateway::{error_message, function_tools, read_stream, send, turns, SseDecoder, Turn};
use crate::model::{Delta, ModelError, ModelRequest, ModelResponse, OnDelta, ToolCallRequest, Usage};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(super) fn body(request: &ModelRequest) -> Value {
	let (system, turns) = turns(&request.messages);
	let mut input = Vec::new();
	for turn in turns {
		match turn {
			Turn::User(text) => input.push(json!({"role":"user","content":text})),
			Turn::Assistant { text, calls } => {
				if !text.is_empty() {
					input.push(json!({"role":"assistant","content":text}));
				}
				for call in calls {
					input.push(json!({
						"type":"function_call",
						"call_id":call.id,
						"name":call.name,
						"arguments":call.arguments
					}));
				}
			}
			Turn::Tool { call_id, content, .. } => input.push(json!({
				"type":"function_call_output",
				"call_id":call_id,
				"output":content
			})),
		}
	}
	let mut body = json!({
		"model": request.model,
		"input": input,
		"stream": true,
		"store": false
	});
	if !system.is_empty() {
		body["instructions"] = json!(system);
	}
	let tools: Vec<Value> = function_tools(&request.tools)
        .into_iter()
        .map(|(name, description, parameters)| {
            json!({"type":"function","name":name,"description":description,"parameters":parameters,"strict":false})
        })
        .collect();
	if !tools.is_empty() {
		body["tools"] = json!(tools);
	}
	if let Some(effort) = &request.effort {
		body["reasoning"] = json!({"effort":effort});
	}
	body
}

#[derive(Default)]
struct Accumulator {
	response: ModelResponse,
	/// Output item ID → position in `response.tool_calls`.
	calls: HashMap<String, usize>,
	finished: bool,
}

impl Accumulator {
	fn call(&mut self, item: &Value) -> usize {
		let id = item["id"].as_str().unwrap_or_default().to_owned();
		if let Some(position) = self.calls.get(&id) {
			return *position;
		}
		self.calls.insert(id, self.response.tool_calls.len());
		self.response.tool_calls.push(ToolCallRequest::default());
		self.response.tool_calls.len() - 1
	}

	fn apply(&mut self, event_name: &str, event: &Value) -> Result<Vec<Delta>, ModelError> {
		let mut deltas = Vec::new();
		let kind = event["type"].as_str().unwrap_or(event_name);
		match kind {
			"response.output_text.delta" => {
				let text = event["delta"].as_str().unwrap_or_default();
				if !text.is_empty() {
					self.response.text.push_str(text);
					deltas.push(Delta::Text(text.to_owned()));
				}
			}
			// Command Code emits the non-standard `response.reasoning.delta`.
			"response.reasoning_text.delta" | "response.reasoning.delta" | "response.reasoning_summary_text.delta" => {
				let text = event["delta"].as_str().unwrap_or_default();
				if !text.is_empty() {
					self.response.reasoning.push_str(text);
					deltas.push(Delta::Reasoning(text.to_owned()));
				}
			}
			"response.output_item.added" | "response.output_item.done" => {
				let item = &event["item"];
				if item["type"] == "function_call" {
					let position = self.call(item);
					let call = &mut self.response.tool_calls[position];
					if let Some(id) = item["call_id"].as_str() {
						call.id = id.to_owned();
					}
					if let Some(name) = item["name"].as_str() {
						call.name = name.to_owned();
					}
					// The completed item carries the full arguments.
					if let Some(arguments) = item["arguments"].as_str().filter(|a| !a.is_empty()) {
						call.arguments = arguments.to_owned();
					}
				}
			}
			"response.function_call_arguments.delta" => {
				let item = json!({"id":event["item_id"]});
				let position = self.call(&item);
				self.response.tool_calls[position]
					.arguments
					.push_str(event["delta"].as_str().unwrap_or_default());
			}
			"response.completed" | "response.incomplete" => {
				let response = &event["response"];
				let usage = &response["usage"];
				if usage.is_object() {
					let number = |value: &Value| value.as_u64().unwrap_or_default();
					let input = number(&usage["input_tokens"]);
					let output = number(&usage["output_tokens"]);
					self.response.usage = Some(Usage {
						input,
						cached: number(&usage["input_tokens_details"]["cached_tokens"]),
						output,
						reasoning: number(&usage["output_tokens_details"]["reasoning_tokens"]),
						total: usage["total_tokens"].as_u64().unwrap_or(input + output),
					});
				}
				self.response.finish_reason = Some(if kind == "response.incomplete" {
					response["incomplete_details"]["reason"]
						.as_str()
						.unwrap_or("incomplete")
						.to_owned()
				} else if self.response.tool_calls.is_empty() {
					"stop".into()
				} else {
					"tool_calls".into()
				});
				self.finished = true;
			}
			"response.failed" => {
				return Err(ModelError::new(
					format!("The model reported an error: {}", error_message(&event["response"])),
					false,
				))
			}
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
}

/// Stream one Responses request. `request` must carry the URL and authentication.
pub(super) async fn stream(
	request: reqwest::RequestBuilder,
	model: &ModelRequest,
	on_delta: OnDelta<'_>,
) -> Result<ModelResponse, ModelError> {
	let response = send(request.json(&body(model))).await?;
	let mut decoder = SseDecoder::default();
	let mut accumulator = Accumulator::default();
	let mut produced = false;
	read_stream(response, |chunk| {
		for event in decoder.push(chunk) {
			if event.data == "[DONE]" {
				return Ok(true);
			}
			let value: Value = serde_json::from_str(&event.data)
				.map_err(|e| ModelError::new(format!("Malformed model stream event: {e}"), false))?;
			for delta in accumulator.apply(&event.event, &value)? {
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
			"The model stream ended before the response was complete",
			!produced,
		));
	}
	Ok(accumulator.response.finish())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn requests_map_turns_to_input_items() {
		let body = body(&ModelRequest {
			model: "gpt".into(),
			messages: vec![
				json!({"role":"system","content":"rules"}),
				json!({"role":"user","content":"hi"}),
				json!({"role":"assistant","content":"","tool_calls":[
					{"id":"c","type":"function","function":{"name":"read_file","arguments":"{}"}}
				]}),
				json!({"role":"tool","tool_call_id":"c","content":"data"}),
			],
			tools: vec![
				json!({"type":"function","function":{"name":"read_file","description":"Read","parameters":{}}}),
			],
			effort: Some("high".into()),
			cwd: "/p".into(),
			session: "rx-1".into(),
		});
		assert_eq!(body["instructions"], "rules");
		let kinds: Vec<_> = body["input"]
			.as_array()
			.unwrap()
			.iter()
			.map(|item| item["type"].as_str().unwrap_or("message"))
			.collect();
		assert_eq!(kinds, ["message", "function_call", "function_call_output"]);
		assert_eq!(body["tools"][0]["name"], "read_file");
		assert_eq!(body["reasoning"]["effort"], "high");
	}

	#[test]
	fn stream_events_build_text_reasoning_calls_and_usage() {
		let mut accumulator = Accumulator::default();
		for event in [
			json!({"type":"response.reasoning.delta","delta":"hmm"}),
			json!({"type":"response.output_text.delta","delta":"Hi"}),
			json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
			json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\""}),
			json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":":\"a\"}"}),
			json!({"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}}),
			json!({"type":"response.completed","response":{"usage":{"input_tokens":4,"output_tokens":3,"total_tokens":7,"output_tokens_details":{"reasoning_tokens":1}}}}),
		] {
			accumulator.apply("", &event).unwrap();
		}
		assert!(accumulator.finished);
		let response = accumulator.response.finish();
		assert_eq!(response.reasoning, "hmm");
		assert_eq!(response.text, "Hi");
		assert_eq!(
			response.tool_calls,
			vec![ToolCallRequest {
				id: "call_1".into(),
				name: "read_file".into(),
				arguments: "{\"path\":\"a\"}".into()
			}]
		);
		assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
		assert_eq!(response.usage.unwrap().reasoning, 1);
		assert!(Accumulator::default()
			.apply(
				"",
				&json!({"type":"response.failed","response":{"error":{"message":"nope"}}})
			)
			.is_err());
	}
}
