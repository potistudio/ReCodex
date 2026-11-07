//! End-to-end harness tests through `Host`, with a scripted model.
use super::{
	ledger::{kind, Ledger},
	testing::{reply, tool_call, Scripted, Step},
	Harness,
};
use crate::{host::Host, model::ModelClient, workspace};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::broadcast;

struct Setup {
	host: Arc<Host>,
	root: String,
	_directory: tempfile::TempDir,
}

fn setup(client: Arc<dyn ModelClient>) -> Setup {
	let directory = tempfile::tempdir().unwrap();
	let root = directory.path().join("project");
	std::fs::create_dir_all(&root).unwrap();
	std::fs::write(root.join("notes.txt"), "remember the milk").unwrap();
	// `project_save` registers the path without the Windows `\\?\` prefix, so match it here.
	let root = std::fs::canonicalize(root)
		.unwrap()
		.to_string_lossy()
		.trim_start_matches("\\\\?\\")
		.to_owned();
	let host = Host::new_with_harness(directory.path().join("data"), |path| {
		Harness::with_client(path, client, Some(8))
	})
	.unwrap();
	workspace::project_save(&host.directory, &host.workspace, root.clone(), None).unwrap();
	Setup {
		host,
		root,
		_directory: directory,
	}
}

async fn next(events: &mut broadcast::Receiver<Value>, method: &str) -> Value {
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let envelope = events.recv().await.unwrap();
			if envelope["event"]["method"] == method {
				return envelope["event"].clone();
			}
		}
	})
	.await
	.unwrap_or_else(|_| panic!("timed out waiting for {method}"))
}

async fn start(setup: &Setup) -> String {
	let started = setup
		.host
		.request(
			false,
			"thread/start",
			json!({"cwd":setup.root,"model":"recodex/test-model"}),
		)
		.await
		.unwrap();
	assert_eq!(started["thread"]["provider"], "recodex");
	started["thread"]["id"].as_str().unwrap().to_owned()
}

async fn prompt(setup: &Setup, id: &str, text: &str) -> Value {
	setup
		.host
		.request(
			false,
			"turn/start",
			json!({"threadId":id,"input":[{"type":"text","text":text}]}),
		)
		.await
		.unwrap()
}

#[tokio::test]
async fn tools_run_after_approval_and_history_is_projected_from_the_ledger() {
	let model = Scripted::new(vec![
		reply(
			"",
			vec![
				tool_call("read", "read_file", json!({"path":"notes.txt"})),
				tool_call(
					"write",
					"write_file",
					json!({"path":"out/todo.txt","content":"buy milk\n"}),
				),
				tool_call("escape", "read_file", json!({"path":"../secret"})),
			],
		),
		reply("Saved the todo.", vec![]),
	]);
	let setup = setup(model.clone());
	let models = setup.host.request(false, "model/list", json!({})).await.unwrap();
	assert!(models["data"]
		.as_array()
		.unwrap()
		.iter()
		.any(|model| model["id"] == "recodex/test-model"));
	let mut events = setup.host.events.subscribe();
	let id = start(&setup).await;
	let turn = prompt(&setup, &id, "Write a todo").await;
	let run = turn["turn"]["id"].as_str().unwrap().to_owned();
	assert!(
		setup
			.host
			.request(
				false,
				"turn/start",
				json!({"threadId":id,"input":[{"type":"text","text":"again"}]})
			)
			.await
			.is_err(),
		"a second turn in a running chat is rejected"
	);

	let approval = next(&mut events, "item/fileChange/requestApproval").await;
	assert_eq!(approval["params"]["turnId"], run);
	assert!(!Path::new(&setup.root).join("out/todo.txt").exists());
	let snapshot = setup.host.snapshot(false);
	assert_eq!(snapshot["approvals"].as_array().unwrap().len(), 1);
	setup
		.host
		.request(
			false,
			"approval/respond",
			json!({"id":approval["id"],"result":{"decision":"accept"}}),
		)
		.await
		.unwrap();
	assert!(
		setup
			.host
			.request(
				false,
				"approval/respond",
				json!({"id":approval["id"],"result":{"decision":"accept"}})
			)
			.await
			.is_err(),
		"an approval accepts one answer"
	);
	let completed = next(&mut events, "turn/completed").await;
	assert_eq!(completed["params"]["turn"]["status"], "completed");
	assert_eq!(
		std::fs::read_to_string(Path::new(&setup.root).join("out/todo.txt")).unwrap(),
		"buy milk\n"
	);

	let requests = model.requests.lock().unwrap().clone();
	assert_eq!(requests.len(), 2);
	let tool_results: Vec<_> = requests[1]
		.iter()
		.filter(|message| message["role"] == "tool")
		.map(|message| message["content"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(tool_results[0], "remember the milk");
	assert!(tool_results[1].starts_with("Created"));
	assert!(tool_results[2].contains("outside this project"));

	let read = setup
		.host
		.request(false, "thread/resume", json!({"threadId":id}))
		.await
		.unwrap();
	let turn = &read["thread"]["turns"][0];
	assert_eq!(turn["status"], "completed");
	let items: Vec<_> = turn["items"]
		.as_array()
		.unwrap()
		.iter()
		.map(|item| {
			format!(
				"{}:{}",
				item["type"].as_str().unwrap(),
				item["status"].as_str().unwrap_or("")
			)
		})
		.collect();
	assert_eq!(
		items,
		[
			"userMessage:",
			"mcpToolCall:completed",
			"fileChange:completed",
			"mcpToolCall:failed",
			"agentMessage:"
		]
	);
	let list = setup
		.host
		.request(false, "thread/list", json!({"cwd":setup.root}))
		.await
		.unwrap();
	assert_eq!(list["data"][0]["preview"], "Write a todo");
}

#[tokio::test]
async fn declines_reach_the_model_and_interrupts_stop_a_pending_request() {
	let model = Scripted::new(vec![
		reply(
			"",
			vec![tool_call("cmd", "run_command", json!({"command":"touch ran"}))],
		),
		reply("Understood.", vec![]),
		Step::Hang,
	]);
	let setup = setup(model.clone());
	let mut events = setup.host.events.subscribe();
	let id = start(&setup).await;
	prompt(&setup, &id, "Run something").await;
	let approval = next(&mut events, "item/commandExecution/requestApproval").await;
	assert_eq!(approval["params"]["command"], "touch ran");
	setup
		.host
		.request(
			false,
			"approval/respond",
			json!({"id":approval["id"],"result":{"decision":"decline"}}),
		)
		.await
		.unwrap();
	assert_eq!(
		next(&mut events, "turn/completed").await["params"]["turn"]["status"],
		"completed"
	);
	assert!(!Path::new(&setup.root).join("ran").exists());
	let declined = model.requests.lock().unwrap()[1]
		.iter()
		.find(|message| message["role"] == "tool")
		.unwrap()["content"]
		.clone();
	assert_eq!(declined, "The user declined this action.");

	let turn = prompt(&setup, &id, "Wait forever").await;
	let run = turn["turn"]["id"].as_str().unwrap().to_owned();
	// Interrupt while the model request is in flight.
	tokio::time::timeout(Duration::from_secs(10), async {
		while model.requests.lock().unwrap().len() < 3 {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.unwrap();
	setup
		.host
		.request(false, "turn/interrupt", json!({"threadId":id,"turnId":run}))
		.await
		.unwrap();
	let completed = next(&mut events, "turn/completed").await;
	assert_eq!(completed["params"]["turn"]["status"], "interrupted");
	assert!(setup.host.snapshot(false)["activeTurns"]
		.as_object()
		.unwrap()
		.is_empty());
	// The second prompt sees the first exchange, including the declined call.
	let third = model.requests.lock().unwrap()[2].clone();
	assert_eq!(third.iter().filter(|m| m["role"] == "user").count(), 2, "{third:?}");
	setup
		.host
		.request(false, "thread/delete", json!({"threadId":id}))
		.await
		.unwrap();
	assert!(setup
		.host
		.request(false, "thread/read", json!({"threadId":id}))
		.await
		.is_err());
}

#[tokio::test]
async fn recovery_closes_runs_left_open_by_a_previous_process() {
	let directory = tempfile::tempdir().unwrap();
	let path = directory.path().join("harness.sqlite3");
	{
		let ledger = Ledger::open(&path).unwrap();
		ledger
			.create_session("rx-old", "/project", Some("recodex/m"), None, json!({}))
			.unwrap();
		for (kind, payload) in [
			(kind::INPUT_ACCEPTED, json!({"inputId":"i","text":"go"})),
			(kind::RUN_STARTED, json!({"runId":"r","inputId":"i"})),
			(
				kind::MESSAGE_ASSISTANT,
				json!({"runId":"r","itemId":"m","text":"","toolCalls":[
					{"id":"c","name":"run_command","arguments":"{\"command\":\"deploy\"}"}
				]}),
			),
			(
				kind::TOOL_STARTED,
				json!({"runId":"r","callId":"c","item":{"id":"t","type":"commandExecution","status":"inProgress"}}),
			),
			(kind::TOOL_DISPATCHED, json!({"runId":"r","callId":"c"})),
		] {
			ledger.append("rx-old", None, kind, payload).unwrap();
		}
	}
	let harness = Harness::with_client(&path, Scripted::new(vec![]), None);
	let thread = harness.read_thread("rx-old").unwrap();
	let turn = &thread["thread"]["turns"][0];
	assert_eq!(turn["status"], "interrupted");
	assert_eq!(turn["items"][1]["status"], "failed");
	let events = harness.ledger().unwrap().events("rx-old").unwrap();
	let result = events.iter().find(|event| event.kind == kind::TOOL_COMPLETED).unwrap();
	assert_eq!(result.payload["status"], "outcomeUnknown");
	let messages = super::context::messages(&events, "/project", u64::MAX);
	assert!(messages.last().unwrap()["content"]
		.as_str()
		.unwrap()
		.contains("outcome is unknown"));
	assert!(harness.ledger().unwrap().unfinished_runs().unwrap().is_empty());
}
