//! Minimal run driver: one prompt, one model request, one reply.
//!
//! It keeps only what a turn needs to stay consistent with the rest of the harness:
//!
//! - the prompt and the run are committed to the ledger before anything is broadcast;
//! - the model sees the context projected from ledger events;
//! - every run ends with `run.finished`, so recovery never finds it unfinished;
//! - the UI receives the same `turn/*` and `item/*` events as the full driver.
//!
//! It deliberately omits the tool loop, approvals, retries, cancellation, the active-run
//! registry, and token usage events. No tools are offered to the model, and any tool call in
//! the response is not recorded, because a recorded call without a result would corrupt the
//! context of later turns.
//!
//! The caller must not start two turns on one chat at once. [`turn`] rejects a chat whose
//! previous run is unfinished (see [`Harness::is_running`]), but the check and the append are
//! not atomic; `Host` serializes them.
use super::{context, ledger::kind, Harness, MODEL_PREFIX, THREAD_PREFIX};
use crate::model::{Delta, ModelRequest};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

struct Run {
	session: String,
	run_id: String,
	cwd: String,
	/// Provider-native model ID (without the ReCodex prefix).
	model: String,
	effort: Option<String>,
}

/// Accept a prompt, record it, and start a run. Returns once the input is committed.
///
/// `model` and `effort` override the chat's saved choice; an empty value means no override.
pub(super) fn turn(
	harness: &Arc<Harness>,
	id: &str,
	text: &str,
	model: Option<&str>,
	effort: Option<&str>,
) -> Result<Value, String> {
	if text.trim().is_empty() {
		return Err("A text message is required".into());
	}

	let ledger = harness.ledger()?;
	let session = ledger.session(id)?.ok_or("This chat no longer exists")?;
	let model = model
		.filter(|model| !model.is_empty())
		.map(str::to_owned)
		.or(session.model.clone())
		.ok_or("Choose a model for this chat")?;
	let provider_model = model
		.strip_prefix(MODEL_PREFIX)
		.ok_or("This chat uses the ReCodex harness. Start a new chat to use another provider.")?
		.to_owned();
	let effort = effort
		.filter(|effort| !effort.is_empty())
		.map(str::to_owned)
		.or(session.effort.clone());
	if harness.is_running(id)? {
		return Err("This chat already has a running turn".into());
	}
	if session.model.as_deref() != Some(model.as_str()) || session.effort != effort {
		ledger.append(id, None, kind::SESSION_MODEL, json!({"model":model,"effort":effort}))?;
	}
	let input_id = Uuid::new_v4().to_string();
	let run_id = Uuid::new_v4().to_string();

	ledger.append(id, None, kind::INPUT_ACCEPTED, json!({"inputId":input_id,"text":text}))?;
	ledger.append(
		id,
		None,
		kind::RUN_STARTED,
		json!({"runId":run_id,"inputId":input_id,"model":model,"effort":effort,"maxSteps":1}),
	)?;

	let turn = json!({"id":run_id,"status":"inProgress","items":[]});
	harness.emit(json!({"method":"turn/started","params":{"threadId":id,"turn":turn}}));
	harness.emit(json!({"method":"item/completed","params":{
		"threadId":id,
		"turnId":run_id,
		"item":{"id":format!("{run_id}-user"),"type":"userMessage","content":[{"type":"text","text":text}]}
	}}));
	tokio::spawn(run(
		harness.clone(),
		Run {
			session: id.to_owned(),
			run_id,
			cwd: session.cwd,
			model: provider_model,
			effort: effort.filter(|effort| effort != "off"),
		},
	));
	Ok(json!({"turn": turn}))
}

async fn run(harness: Arc<Harness>, run: Run) {
	let result = step(&harness, &run).await;
	let (status, error) = match result {
		Ok(()) => ("completed", None),
		Err(error) => ("failed", Some(error)),
	};
	let recorded = harness.ledger().and_then(|ledger| {
		ledger.append(
			&run.session,
			None,
			kind::RUN_FINISHED,
			json!({"runId":run.run_id,"status":status,"error":error}),
		)
	});
	let (status, error) = match recorded {
		Ok(_) => (status, error),
		Err(e) => ("failed", Some(format!("{}; {e}", error.unwrap_or_default()))),
	};
	let mut turn = json!({"id":run.run_id,"status":status,"items":[]});
	if let Some(error) = error {
		turn["error"] = json!({"message":error});
	}
	harness.emit(json!({"method":"turn/completed","params":{"threadId":run.session,"turn":turn}}));
}

/// One model request and its recorded reply.
async fn step(harness: &Arc<Harness>, run: &Run) -> Result<(), String> {
	let ledger = harness.ledger()?;
	let events = ledger.events(&run.session)?;
	let through = events.last().map_or(0, |event| event.seq);
	let step_id = Uuid::new_v4().to_string();
	ledger.append(
		&run.session,
		None,
		kind::STEP_STARTED,
		json!({
			"runId":run.run_id,
			"stepId":step_id,
			"index":0,
			"model":run.model,
			"effort":run.effort,
			"contextVersion":context::VERSION,
			"contextThrough":through,
			"tools":[]
		}),
	)?;
	let request = ModelRequest {
		model: run.model.clone(),
		messages: context::build_messages(&events, &run.cwd, through),
		tools: Vec::new(),
		effort: run.effort.clone(),
		cwd: run.cwd.clone(),
		session: run
			.session
			.strip_prefix(THREAD_PREFIX)
			.unwrap_or(&run.session)
			.to_owned(),
	};
	let item_id = format!("{}-0-message", run.run_id);
	let on_delta = |delta: Delta| match delta {
		Delta::Text(text) => harness.emit(json!({"method":"item/agentMessage/delta","params":{
			"threadId":run.session,"turnId":run.run_id,"itemId":item_id,"delta":text
		}})),
		Delta::Reasoning(text) => harness.emit(json!({"method":"item/reasoning/summaryTextDelta","params":{
			"threadId":run.session,"turnId":run.run_id,
			"itemId":format!("{item_id}-reasoning"),"summaryIndex":0,"delta":text
		}})),
	};
	let response = harness
		.client()?
		.stream(&request, &on_delta)
		.await
		.map_err(|error| error.message)?;
	ledger.append(
		&run.session,
		None,
		kind::MESSAGE_ASSISTANT,
		json!({
			"runId":run.run_id,
			"stepId":step_id,
			"itemId":item_id,
			"text":response.text,
			"reasoning":response.reasoning,
			"toolCalls":[],
			"finishReason":response.finish_reason,
			"usage":response.usage.unwrap_or_default().to_value()
		}),
	)?;
	if !response.text.is_empty() {
		harness.emit(json!({"method":"item/completed","params":{
			"threadId":run.session,
			"turnId":run.run_id,
			"item":{"id":item_id,"type":"agentMessage","text":response.text}
		}}));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::harness::testing::{reply, tool_call, Scripted};
	use std::time::Duration;
	use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

	struct Setup {
		harness: Arc<Harness>,
		model: Arc<Scripted>,
		events: UnboundedReceiver<Value>,
		_directory: tempfile::TempDir,
	}

	fn setup(steps: Vec<crate::harness::testing::Step>) -> Setup {
		let directory = tempfile::tempdir().unwrap();
		let model = Scripted::new(steps);
		let harness = Harness::with_client(&directory.path().join("harness.sqlite3"), model.clone(), None);
		let (sender, events) = unbounded_channel();
		harness.set_emit(Arc::new(move |event| {
			let _ = sender.send(event);
		}));
		harness
			.start_session("rx-1", "/project", Some("recodex/test-model"), None)
			.unwrap();
		Setup {
			harness,
			model,
			events,
			_directory: directory,
		}
	}

	async fn completed(events: &mut UnboundedReceiver<Value>) -> Value {
		tokio::time::timeout(Duration::from_secs(10), async {
			loop {
				let event = events.recv().await.unwrap();
				if event["method"] == "turn/completed" {
					return event;
				}
			}
		})
		.await
		.expect("timed out waiting for turn/completed")
	}

	fn kinds(harness: &Harness) -> Vec<String> {
		harness
			.ledger()
			.unwrap()
			.events("rx-1")
			.unwrap()
			.into_iter()
			.map(|event| event.kind)
			.collect()
	}

	#[tokio::test]
	async fn a_prompt_gets_one_reply_and_the_run_is_closed() {
		let mut setup = setup(vec![reply("hello", vec![])]);
		turn(&setup.harness, "rx-1", "hi", None, None).unwrap();
		let done = completed(&mut setup.events).await;
		assert_eq!(done["params"]["turn"]["status"], "completed");
		assert_eq!(
			kinds(&setup.harness),
			[
				"session.created",
				"input.accepted",
				"run.started",
				"step.started",
				"message.assistant",
				"run.finished"
			]
		);
		assert!(setup.harness.ledger().unwrap().unfinished_runs().unwrap().is_empty());
		let requests = setup.model.requests.lock().unwrap();
		assert_eq!(requests.len(), 1);
		assert_eq!(requests[0].last().unwrap()["content"], "hi");
	}

	#[tokio::test]
	async fn a_tool_call_in_the_reply_is_not_recorded() {
		let mut setup = setup(vec![
			reply("", vec![tool_call("c1", "run_command", json!({"command":"echo"}))]),
			reply("second", vec![]),
		]);
		turn(&setup.harness, "rx-1", "first", None, None).unwrap();
		completed(&mut setup.events).await;
		turn(&setup.harness, "rx-1", "again", None, None).unwrap();
		let done = completed(&mut setup.events).await;
		assert_eq!(done["params"]["turn"]["status"], "completed");
		let requests = setup.model.requests.lock().unwrap();
		assert!(requests[1]
			.iter()
			.all(|message| message["role"] != "tool" && message.get("tool_calls").is_none()));
	}

	#[tokio::test]
	async fn a_model_failure_still_finishes_the_run() {
		let mut setup = setup(vec![]);
		turn(&setup.harness, "rx-1", "hi", None, None).unwrap();
		let done = completed(&mut setup.events).await;
		assert_eq!(done["params"]["turn"]["status"], "failed");
		assert_eq!(done["params"]["turn"]["error"]["message"], "script exhausted");
		assert_eq!(kinds(&setup.harness).last().unwrap(), "run.finished");
		assert!(setup.harness.ledger().unwrap().unfinished_runs().unwrap().is_empty());
	}

	#[tokio::test]
	async fn a_second_turn_is_rejected_while_one_is_unfinished() {
		let setup = setup(vec![crate::harness::testing::Step::Hang]);
		turn(&setup.harness, "rx-1", "hi", None, None).unwrap();
		assert_eq!(
			turn(&setup.harness, "rx-1", "again", None, None).unwrap_err(),
			"This chat already has a running turn"
		);
	}

	#[tokio::test]
	async fn empty_input_and_foreign_models_are_rejected() {
		let setup = setup(vec![]);
		assert!(turn(&setup.harness, "rx-1", "  ", None, None).is_err());
		setup
			.harness
			.start_session("rx-2", "/project", Some("other/model"), None)
			.unwrap_err();
		setup.harness.start_session("rx-3", "/project", None, None).unwrap();
		assert_eq!(
			turn(&setup.harness, "rx-3", "hi", None, None).unwrap_err(),
			"Choose a model for this chat"
		);
	}

	#[tokio::test]
	async fn a_model_override_is_recorded_and_used() {
		let mut setup = setup(vec![reply("ok", vec![])]);
		turn(&setup.harness, "rx-1", "hi", Some("recodex/other"), Some("high")).unwrap();
		completed(&mut setup.events).await;
		let events = setup.harness.ledger().unwrap().events("rx-1").unwrap();
		let change = events.iter().find(|event| event.kind == "session.model").unwrap();
		assert_eq!(change.payload["model"], "recodex/other");
		assert_eq!(change.payload["effort"], "high");
		let session = setup.harness.ledger().unwrap().session("rx-1").unwrap().unwrap();
		assert_eq!(session.model.as_deref(), Some("recodex/other"));
	}

	#[tokio::test]
	async fn the_harness_uses_the_minimal_driver_only_when_selected() {
		let mut setup = setup(vec![reply("minimal", vec![]), reply("full", vec![])]);
		setup.harness.set_minimal_driver(true);
		setup.harness.turn("rx-1", "hi", None, None).unwrap();
		completed(&mut setup.events).await;
		assert!(!kinds(&setup.harness).contains(&"tool.started".to_owned()));
		assert!(setup.model.requests.lock().unwrap()[0]
			.iter()
			.all(|m| m["role"] != "tool"));
		assert_eq!(
			setup.harness.interrupt("rx-1", "any").unwrap_err(),
			"The minimal driver cannot interrupt a turn"
		);
		setup.harness.set_minimal_driver(false);
		setup.harness.turn("rx-1", "again", None, None).unwrap();
		let done = completed(&mut setup.events).await;
		assert_eq!(done["params"]["turn"]["status"], "completed");
		assert!(
			kinds(&setup.harness)
				.iter()
				.filter(|kind| *kind == "step.started")
				.count() == 2
		);
	}

	#[tokio::test]
	async fn a_chat_with_an_unfinished_minimal_run_cannot_be_deleted() {
		let setup = setup(vec![crate::harness::testing::Step::Hang]);
		setup.harness.set_minimal_driver(true);
		setup.harness.turn("rx-1", "hi", None, None).unwrap();
		assert_eq!(
			setup.harness.delete("rx-1").unwrap_err(),
			"Stop the running turn before deleting this chat"
		);
	}
}
