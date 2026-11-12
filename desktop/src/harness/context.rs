//! Context Engine and projections. Both the model-visible messages and the UI transcript are
//! derived from ledger events, so a request can be reconstructed from the events it cites.
use super::{
	ids::ThreadId,
	ledger::{kind, Event},
};
use crate::model::Usage;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Version of the event-to-message transformation; recorded with every step.
///
/// - 2: the project root moved from the system prompt to the start of the first user message.
pub const VERSION: u32 = 2;

/// The system prompt. It holds no per-project text: chat templates render the tool definitions
/// right after it, so anything project-specific here would split the prefix cache before the
/// tools, and every new chat in another project would miss on both.
pub const SYSTEM_PROMPT: &str = "You are a coding agent working in a software project.\n\
     Use the provided tools to inspect and change files and to run commands. Paths are \
     relative to the project root. File changes and commands may require the user's \
     approval; if a call is declined, do not retry it unchanged and explain what you \
     would have done instead. Keep answers concise and report what you changed.";

/// The chat's first user message: the project root, then the user's text.
fn first_user_message(cwd: &str, text: &str) -> String {
	format!("Project root: {cwd}\n\n{text}")
}

/// Model-visible messages for the events up to and including `through`.
pub fn build_messages(events: &[Event], cwd: &str, through: u64) -> Vec<Value> {
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
	let mut messages = vec![json!({"role":"system","content":SYSTEM_PROMPT})];
	let mut first = true;
	for event in &events {
		match event.kind.as_str() {
			kind::RUN_STARTED => {
				let input = event.payload["inputId"].as_str().unwrap_or_default();
				if let Some(text) = inputs.get(input) {
					let content = if first {
						first_user_message(cwd, text)
					} else {
						(*text).to_owned()
					};
					first = false;
					messages.push(json!({"role":"user","content":content}));
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
pub fn build_thread(id: ThreadId, cwd: &str, name: Option<&str>, model: Option<&str>, events: &[Event]) -> Value {
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

	/// Prefix caches can share the system prompt and the tools that follow it across projects
	/// only if the project root first appears in the first user message.
	#[test]
	fn only_the_first_user_message_names_the_project_root() {
		let events = vec![
			event(1, kind::INPUT_ACCEPTED, json!({"inputId":"i1","text":"hi"})),
			event(2, kind::RUN_STARTED, json!({"runId":"r1","inputId":"i1"})),
			event(3, kind::RUN_FINISHED, json!({"runId":"r1","status":"completed"})),
			event(4, kind::INPUT_ACCEPTED, json!({"inputId":"i2","text":"again"})),
			event(5, kind::RUN_STARTED, json!({"runId":"r2","inputId":"i2"})),
		];
		let one = build_messages(&events, "/work/alpha", 5);
		let other = build_messages(&events, "C:\\Users\\someone\\beta", 5);
		assert_eq!(one[0], other[0]);
		assert!(!SYSTEM_PROMPT.contains("alpha"));
		assert_eq!(one[1]["content"], "Project root: /work/alpha\n\nhi");
		assert_eq!(other[1]["content"], "Project root: C:\\Users\\someone\\beta\n\nhi");
		assert_eq!(one[2]["content"], "again");
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
		let messages = build_messages(&events, "/p", 6);
		let roles: Vec<_> = messages.iter().map(|m| m["role"].as_str().unwrap()).collect();
		assert_eq!(roles, ["system", "user", "assistant", "tool", "tool"]);
		assert_eq!(messages[3]["content"], "file");
		assert_eq!(messages[4]["content"], "This tool call was not executed.");
		assert_eq!(super::build_messages(&events, "/p", 3).len(), 2);
		let thread = build_thread("rx-1", "/p", None, None, &events);
		assert_eq!(thread["preview"], "hi");
		assert_eq!(thread["turns"][0]["status"], "interrupted");
		assert_eq!(thread["turns"][0]["items"].as_array().unwrap().len(), 2);
	}

	/// Prompt caching matches a request's leading bytes against earlier requests, so every
	/// request must start with the whole previous one, including across a failed run.
	#[test]
	fn each_request_extends_the_previous_one_as_a_prefix() {
		let events = vec![
			event(1, kind::SESSION_CREATED, json!({})),
			event(2, kind::INPUT_ACCEPTED, json!({"inputId":"i1","text":"read it"})),
			event(3, kind::RUN_STARTED, json!({"runId":"r1","inputId":"i1"})),
			event(
				4,
				kind::MESSAGE_ASSISTANT,
				json!({"runId":"r1","text":"","reasoning":"thinking","toolCalls":[
					{"id":"a","name":"read_file","arguments":"{\"path\":\"x\"}"},
					{"id":"b","name":"list_directory","arguments":"{}"}
				]}),
			),
			event(
				5,
				kind::TOOL_COMPLETED,
				json!({"runId":"r1","callId":"b","content":"dir"}),
			),
			event(
				6,
				kind::TOOL_COMPLETED,
				json!({"runId":"r1","callId":"a","content":"file"}),
			),
			event(
				7,
				kind::MESSAGE_ASSISTANT,
				json!({"runId":"r1","text":"done","toolCalls":[]}),
			),
			event(8, kind::RUN_FINISHED, json!({"runId":"r1","status":"completed"})),
			event(9, kind::INPUT_ACCEPTED, json!({"inputId":"i2","text":"again"})),
			event(10, kind::RUN_STARTED, json!({"runId":"r2","inputId":"i2"})),
			event(11, kind::ATTEMPT_FAILED, json!({"runId":"r2","partialText":"half"})),
			event(12, kind::RUN_FINISHED, json!({"runId":"r2","status":"failed"})),
			event(13, kind::INPUT_ACCEPTED, json!({"inputId":"i3","text":"once more"})),
			event(14, kind::RUN_STARTED, json!({"runId":"r3","inputId":"i3"})),
		];
		// The `through` value of each request: first step, second step after the tool results,
		// the next run, and the run after a failed one.
		let requests: Vec<_> = [3, 6, 10, 14]
			.iter()
			.map(|through| build_messages(&events, "/p", *through))
			.collect();
		for pair in requests.windows(2) {
			let (earlier, later) = (&pair[0], &pair[1]);
			assert!(later.len() > earlier.len());
			assert_eq!(&later[..earlier.len()], earlier.as_slice());
		}
		let roles: Vec<_> = requests[3].iter().map(|m| m["role"].as_str().unwrap()).collect();
		// The failed run keeps its prompt, so two user messages follow each other.
		assert_eq!(
			roles,
			[
				"system",
				"user",
				"assistant",
				"tool",
				"tool",
				"assistant",
				"user",
				"user"
			]
		);
	}
}
