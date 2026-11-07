//! Ignored probe: which parts of a `/alpha/generate` request separate prompt caches. Each
//! variant changes one field of a warmed base request; a variant that reads far fewer cached
//! tokens than the control does not share the base's cache.
//!
//! `cargo test --manifest-path desktop/Cargo.toml --lib generate_probe -- --ignored --nocapture`
use super::{
	command_code::{Store, API_URL},
	gateway::http_client,
	generate_api,
};
use crate::model::ModelRequest;
use serde_json::{json, Value};
use std::time::Duration;

async fn send(key: &str, body: &Value, slug: &str) -> (u64, u64) {
	let text = http_client()
		.unwrap()
		.post(format!("{API_URL}{}", generate_api::PATH))
		.bearer_auth(key)
		.header("x-command-code-version", generate_api::CLI_VERSION)
		.header("x-cli-environment", "production")
		.header("x-project-slug", slug)
		.header("x-taste-learning", "false")
		.header("User-Agent", "cli")
		.json(body)
		.send()
		.await
		.unwrap()
		.text()
		.await
		.unwrap();
	for line in text.lines() {
		let line = line.trim();
		let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
		if let Ok(event) = serde_json::from_str::<Value>(data) {
			if event["type"] == "finish" {
				let usage = &event["totalUsage"];
				return (
					usage["inputTokens"].as_u64().unwrap_or_default(),
					usage["inputTokenDetails"]["cacheReadTokens"]
						.as_u64()
						.unwrap_or_default(),
				);
			}
		}
	}
	panic!("no finish event: {}", &text[..text.len().min(400)]);
}

#[tokio::test]
#[ignore = "calls the real Command Code API"]
async fn generate_cache_partitions() {
	let app = std::path::PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join("com.poti.recodex");
	let (credentials, _) = Store::new(&app).load().expect("signed in");
	let key = credentials.api_key;
	let model = std::env::var("PROBE_MODEL").unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into());
	// A fresh first line keeps earlier runs from warming the base request.
	let nonce = uuid::Uuid::new_v4();
	let rules: String = (1..=80)
		.map(|n| {
			format!(
				"Rule {n}: keep record {} aligned with ledger column {}.\n",
				n * 7919 % 10007,
				n % 17
			)
		})
		.collect();
	let request = |cwd: &str, tail: &str| ModelRequest {
		model: model.clone(),
		messages: vec![
			json!({"role":"system","content":format!("Run {nonce}.\n{rules}{tail}")}),
			json!({"role":"user","content":"Reply with only the word OK."}),
		],
		tools: Vec::new(),
		effort: None,
		cwd: cwd.into(),
		session: "00000000-0000-4000-8000-000000000001".into(),
	};
	let base = generate_api::body(
		&request("C:\\work\\alpha", "Project root: C:\\work\\alpha"),
		"2026-11-07",
	);
	let base_slug = generate_api::project_slug("C:\\work\\alpha");
	let mut variants: Vec<(&str, Value, String)> = vec![
		("cold", base.clone(), base_slug.clone()),
		("control", base.clone(), base_slug.clone()),
		("slug", base.clone(), generate_api::project_slug("C:\\work\\beta")),
	];
	let mut working_dir = base.clone();
	working_dir["config"]["workingDir"] = json!("C:\\work\\beta");
	variants.push(("workingDir", working_dir, base_slug.clone()));
	let mut date = base.clone();
	date["config"]["date"] = json!("2026-11-08");
	variants.push(("date", date, base_slug.clone()));
	let mut thread = base.clone();
	thread["threadId"] = json!(uuid::Uuid::new_v4().to_string());
	variants.push(("threadId", thread, base_slug.clone()));
	let tail = generate_api::body(
		&request("C:\\work\\alpha", "Project root: C:\\work\\beta"),
		"2026-11-07",
	);
	variants.push(("systemTail", tail, base_slug.clone()));
	let mut all = generate_api::body(&request("C:\\work\\beta", "Project root: C:\\work\\beta"), "2026-11-07");
	all["threadId"] = base["threadId"].clone();
	variants.push(("allProject", all, generate_api::project_slug("C:\\work\\beta")));
	for (label, body, slug) in variants {
		let (input, cached) = send(&key, &body, &slug).await;
		println!("VAR {model} {label:<11} input={input} cached={cached}");
		tokio::time::sleep(Duration::from_secs(2)).await;
	}
}

/// Five function tools of roughly the harness's size (about 600 tokens).
fn tools() -> Vec<Value> {
	(1..=5)
		.map(|n| {
			json!({"type":"function","function":{
				"name": format!("tool_{n}"),
				"description": format!("Tool number {n}. {}", "It inspects or changes project files and reports what happened. ".repeat(6)),
				"parameters": {"type":"object","properties":{
					"path":{"type":"string","description":"Path relative to the project root."},
					"content":{"type":"string","description":"Text to write, or a pattern to look for."},
					"limit":{"type":"integer","description":"Maximum number of lines or entries to return."}
				},"required":["path"]}
			}})
		})
		.collect()
}

/// Where the project root sits relative to the tool definitions: at the end of a short
/// system prompt (which the template renders before the tools) or in the first user message.
#[tokio::test]
#[ignore = "calls the real Command Code API"]
async fn project_root_position_with_tools() {
	let app = std::path::PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join("com.poti.recodex");
	let (credentials, _) = Store::new(&app).load().expect("signed in");
	let key = credentials.api_key;
	let model = std::env::var("PROBE_MODEL").unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into());
	let nonce = uuid::Uuid::new_v4();
	let system = format!(
		"Run {nonce}. You are a coding agent working in a software project. Use the provided tools to inspect \
		 and change files and to run commands. Paths are relative to the project root. Keep answers concise."
	);
	let request = |system: String, user: String| ModelRequest {
		model: model.clone(),
		messages: vec![
			json!({"role":"system","content":system}),
			json!({"role":"user","content":user}),
		],
		tools: tools(),
		effort: None,
		cwd: "C:\\work\\alpha".into(),
		session: "00000000-0000-4000-8000-000000000002".into(),
	};
	let ask = "Reply with only the word OK.";
	let slug = generate_api::project_slug("C:\\work\\alpha");
	let variants = [
		(
			"tail-cold",
			request(format!("{system}\n\nProject root: C:\\work\\alpha"), ask.into()),
		),
		(
			"tail-ctrl",
			request(format!("{system}\n\nProject root: C:\\work\\alpha"), ask.into()),
		),
		(
			"tail-beta",
			request(format!("{system}\n\nProject root: C:\\work\\beta"), ask.into()),
		),
		(
			"user-cold",
			request(system.clone(), format!("Project root: C:\\work\\alpha\n\n{ask}")),
		),
		(
			"user-beta",
			request(system.clone(), format!("Project root: C:\\work\\beta\n\n{ask}")),
		),
	];
	for (label, request) in variants {
		let (input, cached) = send(&key, &generate_api::body(&request, "2026-11-07"), &slug).await;
		println!("POS {model} {label:<9} input={input} cached={cached}");
		tokio::time::sleep(Duration::from_secs(2)).await;
	}
}
