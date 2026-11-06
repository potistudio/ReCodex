//! Context Engine and projections. Both the model-visible messages and the UI transcript are
//! derived from ledger events, so a request can be reconstructed from the events it cites.
use super::ledger::{kind, Event};
use super::model::Usage;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Version of the event-to-message transformation; recorded with every step.
pub const VERSION: u32 = 1;

pub fn system_prompt(cwd: &str) -> String {
	format!(
		"You are a coding agent working in the project at {cwd}.\n\
         Use the provided tools to inspect and change files and to run commands. Paths are \
         relative to the project root. File changes and commands may require the user's \
         approval; if a call is declined, do not retry it unchanged and explain what you \
         would have done instead. Keep answers concise and report what you changed."
	)
}

/// Model-visible messages for the events up to and including `through`.
pub fn messages(events: &[Event], cwd: &str, through: u64) -> Vec<Value> {
	let events: Vec<&Event> = events.iter().filter(|e| e.seq <= through).collect();
	let mut inputs = HashMap::new();
	let mut results = HashMap::new();
	for event in &events {
		match event.kind.as_str() {
			kind::INPUT_ACCEPTED => {
				inputs.insert(
					event.payload["inputId"].as_str().unwrap_or_default(),
					event.payload["text"].as_str().unwrap_or_default(),
				);
			}
			kind::TOOL_COMPLETED => {
				results.insert(
					event.payload["callId"].as_str().unwrap_or_default(),
					event.payload["content"].as_str().unwrap_or_default(),
				);
			}
			_ => {}
		}
	}
	let mut messages = vec![json!({"role":"system","content":system_prompt(cwd)})];
	for event in &events {
		match event.kind.as_str() {
			kind::RUN_STARTED => {
				let input = event.payload["inputId"].as_str().unwrap_or_default();
				if let Some(text) = inputs.get(input) {
					messages.push(json!({"role":"user","content":text}));
				}
			}
			kind::MESSAGE_ASSISTANT => {
				let text = event.payload["text"].as_str().unwrap_or_default();
				let calls = event.payload["toolCalls"].as_array().cloned().unwrap_or_default();
				if text.is_empty() && calls.is_empty() {
					continue;
				}
				let mut message = json!({"role":"assistant","content":text});
				if !calls.is_empty() {
					message["tool_calls"] = json!(calls
						.iter()
						.map(|call| json!({
							"id": call["id"],
							"type": "function",
							"function": {"name": call["name"], "arguments": call["arguments"]}
						}))
						.collect::<Vec<_>>());
				}
				messages.push(message);
				// Every call needs a result message, including calls that never ran.
				for call in calls {
					let id = call["id"].as_str().unwrap_or_default();
					let content = results.get(id).copied().unwrap_or("This tool call was not executed.");
					messages.push(json!({"role":"tool","tool_call_id":id,"content":content}));
				}
			}
			_ => {}
		}
	}
	messages
}

pub fn usage_total(events: &[Event]) -> Usage {
	let mut total = Usage::default();
	for event in events {
		if event.kind == kind::MESSAGE_ASSISTANT {
			let usage = Usage::from_value(&event.payload["usage"]);
			total.input += usage.input;
			total.cached += usage.cached;
			total.output += usage.output;
			total.reasoning += usage.reasoning;
			total.total += usage.total;
		}
	}
	total
}

/// The ReCodex transcript view: one turn per run.
pub fn thread(id: &str, cwd: &str, name: Option<&str>, model: Option<&str>, events: &[Event]) -> Value {
	let mut inputs = HashMap::new();
	let mut turns: Vec<Value> = Vec::new();
	let mut runs: HashMap<String, usize> = HashMap::new();
	let mut tools: HashMap<String, (usize, usize)> = HashMap::new();
	let mut preview = String::new();
	let mut updated = 0;
	for event in events {
		updated = updated.max(event.at);
		let run = event.payload["runId"].as_str().unwrap_or_default();
		match event.kind.as_str() {
			kind::INPUT_ACCEPTED => {
				inputs.insert(
					event.payload["inputId"].as_str().unwrap_or_default().to_owned(),
					event.payload["text"].as_str().unwrap_or_default().to_owned(),
				);
			}
			kind::RUN_STARTED => {
				let text = inputs
					.get(event.payload["inputId"].as_str().unwrap_or_default())
					.cloned()
					.unwrap_or_default();
				if preview.is_empty() {
					preview = text.clone();
				}
				runs.insert(run.to_owned(), turns.len());
				turns.push(json!({
					"id": run,
					"status": "inProgress",
					"items": [{
						"id": format!("{run}-user"),
						"type": "userMessage",
						"content": [{"type":"text","text":text}]
					}]
				}));
			}
			kind::MESSAGE_ASSISTANT => {
				let text = event.payload["text"].as_str().unwrap_or_default();
				if let (Some(index), false) = (runs.get(run), text.is_empty()) {
					push(
						&mut turns[*index],
						json!({"id":event.payload["itemId"],"type":"agentMessage","text":text}),
					);
				}
			}
			kind::TOOL_STARTED | kind::TOOL_COMPLETED => {
				let Some(index) = runs.get(run).copied() else {
					continue;
				};
				let call = event.payload["callId"].as_str().unwrap_or_default();
				let item = event.payload["item"].clone();
				match tools.get(call) {
					Some((turn, position)) => turns[*turn]["items"][*position] = item,
					None => {
						let position = turns[index]["items"].as_array().map_or(0, Vec::len);
						push(&mut turns[index], item);
						tools.insert(call.to_owned(), (index, position));
					}
				}
			}
			kind::RUN_FINISHED => {
				if let Some(index) = runs.get(run) {
					turns[*index]["status"] = event.payload["status"].clone();
					if let Some(error) = event.payload["error"].as_str() {
						turns[*index]["error"] = json!({"message":error});
					}
				}
			}
			_ => {}
		}
	}
	json!({
		"id": id,
		"name": name,
		"preview": preview,
		"cwd": cwd,
		"updatedAt": updated,
		"turns": turns,
		"provider": super::PROVIDER,
		"providerLabel": super::PROVIDER_LABEL,
		"model": model
	})
}

fn push(turn: &mut Value, item: Value) {
	if let Some(items) = turn["items"].as_array_mut() {
		items.push(item);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn event(seq: u64, kind: &str, payload: Value) -> Event {
		Event {
			seq,
			kind: kind.into(),
			payload,
			at: seq,
		}
	}

	#[test]
	fn messages_pair_every_tool_call_with_a_result() {
		let events = vec![
			event(1, kind::SESSION_CREATED, json!({})),
			event(2, kind::INPUT_ACCEPTED, json!({"inputId":"i","text":"hi"})),
			event(3, kind::RUN_STARTED, json!({"runId":"r","inputId":"i"})),
			event(
				4,
				kind::MESSAGE_ASSISTANT,
				json!({"runId":"r","itemId":"m","text":"","toolCalls":[
					{"id":"a","name":"read_file","arguments":"{}"},
					{"id":"b","name":"run_command","arguments":"{}"}
				]}),
			),
			event(
				5,
				kind::TOOL_COMPLETED,
				json!({"runId":"r","callId":"a","content":"file","item":{"id":"t"}}),
			),
			event(6, kind::RUN_FINISHED, json!({"runId":"r","status":"interrupted"})),
		];
		let messages = messages(&events, "/p", 6);
		let roles: Vec<_> = messages.iter().map(|m| m["role"].as_str().unwrap()).collect();
		assert_eq!(roles, ["system", "user", "assistant", "tool", "tool"]);
		assert_eq!(messages[3]["content"], "file");
		assert_eq!(messages[4]["content"], "This tool call was not executed.");
		assert_eq!(super::messages(&events, "/p", 3).len(), 2);
		let thread = thread("rx-1", "/p", None, None, &events);
		assert_eq!(thread["preview"], "hi");
		assert_eq!(thread["turns"][0]["status"], "interrupted");
		assert_eq!(thread["turns"][0]["items"].as_array().unwrap().len(), 2);
	}
}
