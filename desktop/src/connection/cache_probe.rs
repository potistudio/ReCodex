//! Prompt-cache probe against the real Command Code API, using the desktop app's stored
//! sign-in. Ignored by default. Run one scenario at a time:
//!
//! `PROBE_SCENARIO=turns cargo test --manifest-path desktop/Cargo.toml --lib cache_probe -- --ignored --nocapture`
//!
//! Scenarios: `turns`, `random-thread`, `tools`, `cross`, `gap` (`PROBE_GAP` seconds).
//! `PROBE_MODEL` defaults to `deepseek/deepseek-v4-flash`. Each request prints one `REQ` line:
//! `ideal` is the previous request's input in the same chat rounded down to 128 tokens, the most
//! a prefix cache with 128-token blocks can serve; `loss` is how far `cached` falls short of it.
use super::*;
use std::{
	collections::HashMap,
	time::{Duration, Instant},
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const BLOCK: u64 = 128;

struct Probe {
	harness: Arc<Harness>,
	_connection: Arc<Connection>,
	events: UnboundedReceiver<Value>,
	model: String,
	scenario: String,
	previous: HashMap<String, u64>,
	input: u64,
	cached: u64,
	loss: u64,
	directory: tempfile::TempDir,
}

impl Probe {
	fn new(scenario: &str) -> Self {
		let app = std::path::PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join("com.poti.recodex");
		let directory = tempfile::tempdir().unwrap();
		let harness = Harness::new(&directory.path().join("harness.sqlite3"), None);
		let connection = Connection::for_test(
			harness.clone(),
			Config::default(),
			Store::new(&app),
			command_code::STUDIO_URL,
			command_code::API_URL,
		);
		let (sender, events) = unbounded_channel();
		harness.set_emit(Arc::new(move |event| {
			let _ = sender.send(event);
		}));
		Self {
			harness,
			_connection: connection,
			events,
			model: std::env::var("PROBE_MODEL").unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into()),
			scenario: scenario.into(),
			previous: HashMap::new(),
			input: 0,
			cached: 0,
			loss: 0,
			directory,
		}
	}

	/// A project directory with three deterministic documents of about 1,500 tokens each.
	fn project(&self, name: &str) -> String {
		let root = self.directory.path().join(name);
		std::fs::create_dir_all(root.join("docs")).unwrap();
		for (index, file) in ["a", "b", "c"].iter().enumerate() {
			let body: String = (1..=120)
				.map(|n| {
					format!(
						"{file}-{n}: shipment {} left depot {} with {} crates.\n",
						(n * 7919 + index * 31) % 10007,
						(n * 13 + index) % 97,
						(n * 104729) % 991
					)
				})
				.collect();
			std::fs::write(root.join("docs").join(format!("{file}.md")), body).unwrap();
		}
		root.to_string_lossy().into_owned()
	}

	fn chat_with_id(&self, id: &str, root: &str) -> String {
		self.harness
			.start_session(id, root, Some(&format!("recodex/{}", self.model)), None)
			.unwrap();
		id.to_owned()
	}

	fn chat(&self, root: &str) -> String {
		self.chat_with_id(&format!("rx-{}", uuid::Uuid::new_v4()), root)
	}

	async fn turn(&mut self, chat: &str, label: &str, text: &str) {
		let started = Instant::now();
		self.harness.turn(chat, text, None, None).unwrap();
		let mut step = 0;
		loop {
			let event = tokio::time::timeout(Duration::from_secs(300), self.events.recv())
				.await
				.expect("timed out")
				.expect("channel closed");
			let method = event["method"].as_str().unwrap_or_default();
			if method.ends_with("requestApproval") {
				let id = event["id"].as_str().unwrap();
				self.harness.respond(id, &json!({"decision":"decline"})).unwrap();
				println!("NOTE {label} declined {method}");
				continue;
			}
			match method {
				"thread/tokenUsage/updated" => {
					step += 1;
					let last = &event["params"]["tokenUsage"]["last"];
					let input = last["inputTokens"].as_u64().unwrap_or_default();
					let cached = last["cachedInputTokens"].as_u64().unwrap_or_default();
					let output = last["outputTokens"].as_u64().unwrap_or_default();
					let ideal = self.previous.get(chat).map(|previous| previous / BLOCK * BLOCK);
					let loss = ideal.map_or(0, |ideal| ideal.saturating_sub(cached));
					self.previous.insert(chat.to_owned(), input);
					self.input += input;
					self.cached += cached;
					self.loss += loss;
					println!(
						"REQ {} {} {label} step={step} input={input} cached={cached} hit={:.1}% output={output} ideal={} loss={loss} ms={}",
						self.scenario,
						self.model,
						100.0 * cached as f64 / input.max(1) as f64,
						ideal.map_or("-".into(), |ideal| ideal.to_string()),
						started.elapsed().as_millis()
					);
				}
				"turn/completed" => {
					let turn = &event["params"]["turn"];
					if turn["status"] != "completed" {
						println!(
							"NOTE {label} status={} error={}",
							turn["status"], turn["error"]["message"]
						);
					}
					break;
				}
				_ => {}
			}
		}
	}

	fn summary(&self) {
		println!(
			"SUM {} {} input={} cached={} hit={:.1}% loss={}",
			self.scenario,
			self.model,
			self.input,
			self.cached,
			100.0 * self.cached as f64 / self.input.max(1) as f64,
			self.loss
		);
	}
}

fn padding() -> String {
	(1..=150)
		.map(|n| {
			format!(
				"Reference line {n}: archive record {n} lists item {} and checksum {}.\n",
				n * 7919 % 10007,
				n * 104729 % 99991
			)
		})
		.collect()
}

async fn short_turns(probe: &mut Probe, chat: &str, gap: Duration) {
	let long = format!("{}\nIgnore the list above. Reply with only the word OK.", padding());
	probe.turn(chat, "t1", &long).await;
	for (index, text) in [
		"Reply with only the word OK again.",
		"Reply with only OK.",
		"Reply with only OK, once more.",
	]
	.iter()
	.enumerate()
	{
		tokio::time::sleep(gap).await;
		probe.turn(chat, &format!("t{}", index + 2), text).await;
	}
}

#[tokio::test]
#[ignore = "calls the real Command Code API"]
async fn probe() {
	let scenario = std::env::var("PROBE_SCENARIO").unwrap_or_else(|_| "turns".into());
	let mut probe = Probe::new(&scenario);
	let root = probe.project("alpha");
	match scenario.as_str() {
		"turns" => {
			let chat = probe.chat(&root);
			short_turns(&mut probe, &chat, Duration::from_secs(3)).await;
		}
		// A chat ID that is not a UUID gives every request a fresh `threadId`.
		"random-thread" => {
			let chat = probe.chat_with_id("rx-probe-random", &root);
			short_turns(&mut probe, &chat, Duration::from_secs(3)).await;
		}
		"gap" => {
			let gap = std::env::var("PROBE_GAP")
				.ok()
				.and_then(|v| v.parse().ok())
				.unwrap_or(120);
			let chat = probe.chat(&root);
			short_turns(&mut probe, &chat, Duration::from_secs(gap)).await;
		}
		"tools" => {
			let chat = probe.chat(&root);
			probe
				.turn(
					&chat,
					"t1",
					"List the docs directory, then read docs/a.md, docs/b.md and docs/c.md with one read_file call per \
					 step, in that order. Do not run commands or change files. Finally answer with only the first \
					 line of each file.",
				)
				.await;
			probe
				.turn(&chat, "t2", "Read docs/b.md again and answer with only its last line.")
				.await;
		}
		// First requests of new chats: same project twice, then another project.
		"cross" => {
			let other = probe.project("beta-project-with-a-longer-name");
			for (label, root, text) in [
				("alpha-1", &root, "Reply with only the word OK."),
				("alpha-2", &root, "Reply with only the word YES."),
				("beta-1", &other, "Reply with only the word NO."),
				("beta-2", &other, "Reply with only the word MAYBE."),
			] {
				let chat = probe.chat(root);
				probe.turn(&chat, label, text).await;
				tokio::time::sleep(Duration::from_secs(3)).await;
			}
		}
		other => panic!("unknown scenario {other}"),
	}
	probe.summary();
}
