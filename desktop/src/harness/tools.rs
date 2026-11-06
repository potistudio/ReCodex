//! Tool Registry, Tool Scheduler, and the local Execution Environment.
//!
//! A model tool call is validated and normalized into an immutable `PreparedCall` that carries
//! its resource effect. The scheduler batches calls by those effects; execution happens only
//! after authorization, inside the project root.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
	path::{Component, Path, PathBuf},
	process::Stdio,
	time::Duration,
};
use tokio::{
	io::{AsyncRead, AsyncReadExt},
	process::Command,
	sync::{mpsc, watch},
};

use super::model::ToolCallRequest;

const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
/// Model-visible tool results are bounded; the full output stays in the ledger.
pub const MAX_MODEL_OUTPUT: usize = 32 * 1024;
const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
	ReadFile,
	ListDirectory,
	WriteFile,
	EditFile,
	RunCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
	Read,
	Write,
	/// The effect cannot be determined, so the call runs alone.
	Exclusive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
	pub resource: String,
	pub access: Access,
}

pub struct ToolDefinition {
	pub kind: ToolKind,
	pub name: &'static str,
	pub description: &'static str,
	pub parameters: Value,
}

/// The tool set visible to one step.
pub fn registry() -> Vec<ToolDefinition> {
	vec![
		ToolDefinition {
			kind: ToolKind::ReadFile,
			name: "read_file",
			description:
				"Read a UTF-8 text file in the project. Use offset and limit (1-based line numbers) for large files.",
			parameters: json!({
				"type":"object",
				"properties":{
					"path":{"type":"string","description":"Path relative to the project root"},
					"offset":{"type":"integer","minimum":1},
					"limit":{"type":"integer","minimum":1}
				},
				"required":["path"],
				"additionalProperties":false
			}),
		},
		ToolDefinition {
			kind: ToolKind::ListDirectory,
			name: "list_directory",
			description: "List the entries of a directory in the project. Directories end with '/'.",
			parameters: json!({
				"type":"object",
				"properties":{"path":{"type":"string","description":"Path relative to the project root; '.' for the root"}},
				"required":["path"],
				"additionalProperties":false
			}),
		},
		ToolDefinition {
			kind: ToolKind::WriteFile,
			name: "write_file",
			description: "Create or overwrite a UTF-8 text file in the project. Parent directories are created.",
			parameters: json!({
				"type":"object",
				"properties":{
					"path":{"type":"string"},
					"content":{"type":"string"}
				},
				"required":["path","content"],
				"additionalProperties":false
			}),
		},
		ToolDefinition {
			kind: ToolKind::EditFile,
			name: "edit_file",
			description: "Replace exact text in a file. old_text must match exactly once unless replace_all is true.",
			parameters: json!({
				"type":"object",
				"properties":{
					"path":{"type":"string"},
					"old_text":{"type":"string"},
					"new_text":{"type":"string"},
					"replace_all":{"type":"boolean"}
				},
				"required":["path","old_text","new_text"],
				"additionalProperties":false
			}),
		},
		ToolDefinition {
			kind: ToolKind::RunCommand,
			name: "run_command",
			description: "Run a shell command in the project root and return its combined output and exit code.",
			parameters: json!({
				"type":"object",
				"properties":{
					"command":{"type":"string"},
					"timeout_seconds":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT}
				},
				"required":["command"],
				"additionalProperties":false
			}),
		},
	]
}

/// Chat Completions `tools` declarations for a tool set.
pub fn schemas(tools: &[ToolDefinition]) -> Vec<Value> {
	tools
		.iter()
		.map(|tool| {
			json!({
				"type":"function",
				"function":{"name":tool.name,"description":tool.description,"parameters":tool.parameters}
			})
		})
		.collect()
}

#[derive(Clone, Debug)]
pub struct PreparedCall {
	pub kind: ToolKind,
	pub args: Value,
	/// Project-relative path for file tools, normalized with '/' separators.
	pub path: Option<String>,
	pub target: Option<PathBuf>,
	pub effect: Effect,
	/// Hash of the normalized action; an approval applies only to this exact action.
	pub fingerprint: String,
}

/// Validate a model tool call against the registry and normalize it inside `root`.
pub fn prepare(call: &ToolCallRequest, root: &Path) -> Result<PreparedCall, String> {
	let tool = registry()
		.into_iter()
		.find(|tool| tool.name == call.name)
		.ok_or_else(|| format!("Unknown tool {}", call.name))?;
	let args: Value = if call.arguments.trim().is_empty() {
		json!({})
	} else {
		serde_json::from_str(&call.arguments).map_err(|e| format!("Tool arguments are not valid JSON: {e}"))?
	};
	if !args.is_object() {
		return Err("Tool arguments must be a JSON object".into());
	}
	let properties = tool.parameters["properties"].as_object().unwrap();
	for key in args.as_object().unwrap().keys() {
		if !properties.contains_key(key) {
			return Err(format!("Unknown argument {key}"));
		}
	}
	for required in tool.parameters["required"].as_array().unwrap() {
		let key = required.as_str().unwrap();
		if !args[key].is_string() {
			return Err(format!("Argument {key} must be a string"));
		}
	}
	for (key, schema) in properties {
		let value = &args[key.as_str()];
		if value.is_null() {
			continue;
		}
		let valid = match schema["type"].as_str() {
			Some("string") => value.is_string(),
			Some("integer") => value.as_u64().is_some_and(|n| n >= 1),
			Some("boolean") => value.is_boolean(),
			_ => true,
		};
		if !valid {
			return Err(format!("Argument {key} has the wrong type"));
		}
	}
	let (path, target, effect) = match tool.kind {
		ToolKind::RunCommand => (
			None,
			None,
			Effect {
				resource: "*".into(),
				access: Access::Exclusive,
			},
		),
		kind => {
			let relative = normalize(args["path"].as_str().unwrap_or_default())?;
			let target = resolve(root, &relative)?;
			let access = if matches!(kind, ToolKind::ReadFile | ToolKind::ListDirectory) {
				Access::Read
			} else {
				Access::Write
			};
			(
				Some(relative.clone()),
				Some(target),
				Effect {
					resource: format!("file:{relative}"),
					access,
				},
			)
		}
	};
	let fingerprint = format!(
		"{:x}",
		Sha256::digest(json!({"tool":tool.name,"args":args,"path":path}).to_string())
	);
	Ok(PreparedCall {
		kind: tool.kind,
		args,
		path,
		target,
		effect,
		fingerprint,
	})
}

/// Lexically normalize a project-relative path and reject escapes from the root.
fn normalize(path: &str) -> Result<String, String> {
	let path = path.trim();
	if path.is_empty() {
		return Err("A path is required".into());
	}
	if Path::new(path).is_absolute() || path.starts_with('/') || path.starts_with('\\') {
		return Err("Use a path relative to the project root".into());
	}
	let mut parts: Vec<String> = Vec::new();
	for component in Path::new(&path.replace('\\', "/")).components() {
		match component {
			Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
			Component::CurDir => {}
			Component::ParentDir => {
				if parts.pop().is_none() {
					return Err("The path is outside this project".into());
				}
			}
			_ => return Err("Use a path relative to the project root".into()),
		}
	}
	Ok(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

/// Resolve against the real filesystem: the nearest existing ancestor (after following
/// symbolic links) must stay inside the canonical project root.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
	let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
	let joined = if relative == "." {
		root.clone()
	} else {
		root.join(relative)
	};
	let mut existing = joined.as_path();
	let mut rest = Vec::new();
	while !existing.exists() {
		rest.push(existing.file_name().map(PathBuf::from).unwrap_or_default());
		existing = existing.parent().ok_or("The path is outside this project")?;
	}
	let mut resolved = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
	if !resolved.starts_with(&root) {
		return Err("The path is outside this project".into());
	}
	for part in rest.into_iter().rev() {
		resolved.push(part);
	}
	Ok(resolved)
}

fn conflicts(left: &Effect, right: &Effect) -> bool {
	left.access == Access::Exclusive
		|| right.access == Access::Exclusive
		|| (left.resource == right.resource && (left.access == Access::Write || right.access == Access::Write))
}

/// Group calls into ordered batches. Calls in a batch have no conflicting effects and may run
/// concurrently; a conflict, or an exclusive call, starts a new batch.
pub fn schedule(calls: &[PreparedCall]) -> Vec<Vec<usize>> {
	let mut batches: Vec<Vec<usize>> = Vec::new();
	for (index, call) in calls.iter().enumerate() {
		let fits = batches.last().is_some_and(|batch| {
			batch
				.iter()
				.all(|other| !conflicts(&calls[*other].effect, &call.effect))
		});
		if fits {
			batches.last_mut().unwrap().push(index);
		} else {
			batches.push(vec![index]);
		}
	}
	batches
}

#[derive(Clone, Debug)]
pub struct Outcome {
	pub failed: bool,
	pub output: String,
	pub diff: Option<String>,
	pub exit_code: Option<i32>,
}

impl Outcome {
	fn ok(output: String) -> Self {
		Self {
			failed: false,
			output,
			diff: None,
			exit_code: None,
		}
	}
	fn failed(output: String) -> Self {
		Self {
			failed: true,
			output,
			diff: None,
			exit_code: None,
		}
	}
}

/// Execute an authorized call. `on_output` receives the accumulated output of long-running calls.
pub async fn execute(
	call: &PreparedCall,
	root: &Path,
	cancel: watch::Receiver<bool>,
	on_output: &(dyn Fn(&str) + Send + Sync),
) -> Outcome {
	let target = call.target.clone().unwrap_or_default();
	let result = match call.kind {
		ToolKind::ReadFile => read_file(&target, &call.args),
		ToolKind::ListDirectory => list_directory(&target),
		ToolKind::WriteFile => write_file(&target, call.args["content"].as_str().unwrap()),
		ToolKind::EditFile => edit_file(&target, &call.args),
		ToolKind::RunCommand => return run_command(call, root, cancel, on_output).await,
	};
	result.unwrap_or_else(Outcome::failed)
}

fn read_text(path: &Path) -> Result<String, String> {
	let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
	if !metadata.is_file() {
		return Err("The path is not a file".into());
	}
	if metadata.len() > MAX_FILE {
		return Err("Files larger than 2 MB cannot be read with this tool".into());
	}
	let text = std::fs::read_to_string(path).map_err(|_| "This file is not UTF-8 text")?;
	if text.contains('\0') {
		return Err("Binary files cannot be read with this tool".into());
	}
	Ok(text)
}

fn read_file(path: &Path, args: &Value) -> Result<Outcome, String> {
	let text = read_text(path)?;
	let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
	let limit = args["limit"].as_u64().unwrap_or(2000) as usize;
	let lines: Vec<&str> = text.lines().collect();
	if offset > lines.len().max(1) {
		return Err(format!("offset {offset} is past the end ({} lines)", lines.len()));
	}
	let end = (offset - 1 + limit).min(lines.len());
	let mut output = lines[offset - 1..end].join("\n");
	if end < lines.len() {
		output.push_str(&format!(
			"\n[Showing lines {offset}-{end} of {}. Use offset to read more.]",
			lines.len()
		));
	}
	Ok(Outcome::ok(output))
}

fn list_directory(path: &Path) -> Result<Outcome, String> {
	let mut entries = Vec::new();
	for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
		let entry = entry.map_err(|e| e.to_string())?;
		let name = entry.file_name().to_string_lossy().into_owned();
		if name == ".git" {
			continue;
		}
		let directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
		entries.push(if directory { format!("{name}/") } else { name });
	}
	entries.sort_by_key(|name| (!name.ends_with('/'), name.to_lowercase()));
	let total = entries.len();
	entries.truncate(1000);
	let mut output = entries.join("\n");
	if total > 1000 {
		output.push_str(&format!("\n[{} more entries omitted]", total - 1000));
	}
	Ok(Outcome::ok(output))
}

fn write_file(path: &Path, content: &str) -> Result<Outcome, String> {
	if content.len() as u64 > MAX_FILE {
		return Err("Files larger than 2 MB cannot be written with this tool".into());
	}
	let previous = if path.exists() { Some(read_text(path)?) } else { None };
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
	}
	std::fs::write(path, content).map_err(|e| e.to_string())?;
	let mut outcome = Outcome::ok(format!(
		"{} {} ({} bytes)",
		if previous.is_some() { "Updated" } else { "Created" },
		path.file_name().unwrap_or_default().to_string_lossy(),
		content.len()
	));
	outcome.diff = Some(diff(previous.as_deref().unwrap_or_default(), content));
	Ok(outcome)
}

fn edit_file(path: &Path, args: &Value) -> Result<Outcome, String> {
	let old_text = args["old_text"].as_str().unwrap();
	let new_text = args["new_text"].as_str().unwrap();
	if old_text.is_empty() {
		return Err("old_text must not be empty".into());
	}
	let previous = read_text(path)?;
	let count = previous.matches(old_text).count();
	let replace_all = args["replace_all"].as_bool().unwrap_or(false);
	if count == 0 {
		return Err("old_text was not found in the file".into());
	}
	if count > 1 && !replace_all {
		return Err(format!(
			"old_text matches {count} times; add more context or set replace_all"
		));
	}
	let content = if replace_all {
		previous.replace(old_text, new_text)
	} else {
		previous.replacen(old_text, new_text, 1)
	};
	std::fs::write(path, &content).map_err(|e| e.to_string())?;
	let mut outcome = Outcome::ok(format!("Replaced {count} occurrence(s)"));
	outcome.diff = Some(diff(&previous, &content));
	Ok(outcome)
}

/// A bounded single-hunk diff: common leading and trailing lines are elided.
pub fn diff(before: &str, after: &str) -> String {
	let old: Vec<&str> = before.lines().collect();
	let new: Vec<&str> = after.lines().collect();
	let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
	let suffix = old[prefix..]
		.iter()
		.rev()
		.zip(new[prefix..].iter().rev())
		.take_while(|(a, b)| a == b)
		.count();
	let removed = &old[prefix..old.len() - suffix];
	let added = &new[prefix..new.len() - suffix];
	let mut lines = vec![format!(
		"@@ -{},{} +{},{} @@",
		prefix + 1,
		removed.len(),
		prefix + 1,
		added.len()
	)];
	lines.extend(removed.iter().map(|line| format!("-{line}")));
	lines.extend(added.iter().map(|line| format!("+{line}")));
	let mut diff = lines.join("\n");
	truncate(&mut diff, 200_000);
	diff
}

fn truncate(text: &mut String, limit: usize) {
	if text.len() > limit {
		let mut end = limit;
		while !text.is_char_boundary(end) {
			end -= 1;
		}
		text.truncate(end);
		text.push_str("\n[truncated]");
	}
}

/// Keep the head and tail of long output for the model.
pub fn model_output(output: &str) -> String {
	if output.len() <= MAX_MODEL_OUTPUT {
		return output.to_owned();
	}
	let half = MAX_MODEL_OUTPUT / 2;
	let mut head = half;
	while !output.is_char_boundary(head) {
		head -= 1;
	}
	let mut tail = output.len() - half;
	while !output.is_char_boundary(tail) {
		tail += 1;
	}
	format!(
		"{}\n[{} bytes omitted]\n{}",
		&output[..head],
		tail - head,
		&output[tail..]
	)
}

async fn pump(mut reader: impl AsyncRead + Unpin, sender: mpsc::UnboundedSender<Vec<u8>>) {
	let mut buffer = [0u8; 8192];
	while let Ok(read) = reader.read(&mut buffer).await {
		if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
			break;
		}
	}
}

async fn run_command(
	call: &PreparedCall,
	root: &Path,
	mut cancel: watch::Receiver<bool>,
	on_output: &(dyn Fn(&str) + Send + Sync),
) -> Outcome {
	let command_text = call.args["command"].as_str().unwrap_or_default();
	let timeout = call.args["timeout_seconds"]
		.as_u64()
		.unwrap_or(DEFAULT_TIMEOUT)
		.clamp(1, MAX_TIMEOUT);
	#[cfg(windows)]
	let mut command = {
		let mut command = Command::new("powershell.exe");
		command.args(["-NoProfile", "-NonInteractive", "-Command", command_text]);
		command.creation_flags(0x08000000);
		command
	};
	#[cfg(not(windows))]
	let mut command = {
		let mut command = Command::new("/bin/sh");
		command.args(["-c", command_text]);
		command
	};
	command
		.current_dir(root)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true);
	let mut child = match command.spawn() {
		Ok(child) => child,
		Err(e) => return Outcome::failed(format!("The command could not start: {e}")),
	};
	let (sender, mut receiver) = mpsc::unbounded_channel();
	tokio::spawn(pump(child.stdout.take().unwrap(), sender.clone()));
	tokio::spawn(pump(child.stderr.take().unwrap(), sender));
	let mut output = Vec::new();
	let deadline = tokio::time::sleep(Duration::from_secs(timeout));
	tokio::pin!(deadline);
	let mut note = None;
	loop {
		tokio::select! {
			chunk = receiver.recv() => match chunk {
				Some(chunk) => {
					if output.len() < MAX_OUTPUT {
						output.extend_from_slice(&chunk);
						on_output(&String::from_utf8_lossy(&output));
					}
				}
				None => break,
			},
			_ = &mut deadline => {
				note = Some(format!("[Stopped after {timeout} seconds]"));
				break;
			}
			_ = cancel.wait_for(|cancelled| *cancelled) => {
				note = Some("[Stopped because the turn was interrupted]".to_owned());
				break;
			}
		}
	}
	if note.is_some() {
		let _ = child.kill().await;
	}
	let status = child.wait().await.ok();
	let mut text = String::from_utf8_lossy(&output).trim_end().to_owned();
	truncate(&mut text, MAX_OUTPUT);
	let code = status.and_then(|status| status.code());
	let mut parts = vec![text];
	parts.extend(note.clone());
	parts.push(match code {
		Some(code) => format!("[exit code {code}]"),
		None => "[terminated]".to_owned(),
	});
	parts.retain(|part| !part.is_empty());
	Outcome {
		failed: note.is_some() || code != Some(0),
		output: parts.join("\n"),
		diff: None,
		exit_code: code,
	}
}

/// The ReCodex UI item for a call in a given state.
pub fn display_item(
	item_id: &str,
	call: &ToolCallRequest,
	prepared: Option<&PreparedCall>,
	status: &str,
	outcome: Option<&Outcome>,
) -> Value {
	let args: Value = prepared
		.map(|prepared| prepared.args.clone())
		.or_else(|| serde_json::from_str(&call.arguments).ok())
		.unwrap_or(Value::Null);
	let output = outcome.map(|outcome| outcome.output.as_str()).unwrap_or("");
	match call.name.as_str() {
		"run_command" => json!({
			"id": item_id,
			"type": "commandExecution",
			"command": args["command"].as_str().unwrap_or_default(),
			"status": status,
			"aggregatedOutput": output,
			"exitCode": outcome.and_then(|outcome| outcome.exit_code)
		}),
		"write_file" | "edit_file" => {
			let path = prepared
				.and_then(|prepared| prepared.path.clone())
				.or_else(|| args["path"].as_str().map(str::to_owned))
				.unwrap_or_default();
			let diff = outcome.and_then(|outcome| outcome.diff.clone()).unwrap_or_else(|| {
				if call.name == "write_file" {
					diff("", args["content"].as_str().unwrap_or_default())
				} else {
					diff(
						args["old_text"].as_str().unwrap_or_default(),
						args["new_text"].as_str().unwrap_or_default(),
					)
				}
			});
			json!({
				"id": item_id,
				"type": "fileChange",
				"status": status,
				"changes": [{
					"path": path,
					"kind": {"type": if call.name == "write_file" { "add" } else { "update" }},
					"diff": diff
				}]
			})
		}
		name => json!({
			"id": item_id,
			"type": "mcpToolCall",
			"tool": name,
			"server": "recodex",
			"status": status,
			"aggregatedOutput": output
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn call(name: &str, arguments: Value) -> ToolCallRequest {
		ToolCallRequest {
			id: format!("{name}-id"),
			name: name.into(),
			arguments: arguments.to_string(),
		}
	}

	#[test]
	fn calls_are_validated_and_kept_inside_the_root() {
		let directory = tempfile::tempdir().unwrap();
		let root = directory.path().join("project");
		std::fs::create_dir_all(root.join("src")).unwrap();
		std::fs::write(directory.path().join("secret.txt"), "secret").unwrap();
		assert!(prepare(&call("read_file", json!({"path":"../secret.txt"})), &root).is_err());
		assert!(prepare(&call("read_file", json!({"path":"/etc/passwd"})), &root).is_err());
		assert!(prepare(&call("read_file", json!({})), &root).is_err());
		assert!(prepare(&call("read_file", json!({"path":"a","extra":1})), &root).is_err());
		assert!(prepare(&call("missing", json!({})), &root).is_err());
		let prepared = prepare(
			&call("write_file", json!({"path":"src/./new/../a.txt","content":""})),
			&root,
		)
		.unwrap();
		assert_eq!(prepared.path.as_deref(), Some("src/a.txt"));
		assert_eq!(prepared.effect.access, Access::Write);
		#[cfg(unix)]
		{
			std::os::unix::fs::symlink(directory.path(), root.join("escape")).unwrap();
			assert!(prepare(&call("read_file", json!({"path":"escape/secret.txt"})), &root).is_err());
		}
	}

	#[test]
	fn scheduling_parallelizes_reads_and_isolates_conflicts() {
		let directory = tempfile::tempdir().unwrap();
		let root = directory.path();
		let calls: Vec<_> = [
			call("read_file", json!({"path":"a"})),
			call("read_file", json!({"path":"b"})),
			call("write_file", json!({"path":"c","content":""})),
			call("edit_file", json!({"path":"a","old_text":"x","new_text":"y"})),
			call("run_command", json!({"command":"true"})),
			call("read_file", json!({"path":"a"})),
		]
		.iter()
		.map(|c| prepare(c, root).unwrap())
		.collect();
		assert_eq!(schedule(&calls), vec![vec![0, 1, 2], vec![3], vec![4], vec![5]]);
	}

	#[tokio::test]
	async fn file_tools_and_commands_execute_in_the_root() {
		let directory = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(directory.path()).unwrap();
		let (_cancel, receiver) = watch::channel(false);
		let run = |c: ToolCallRequest| {
			let root = root.clone();
			let receiver = receiver.clone();
			async move {
				let prepared = prepare(&c, &root).unwrap();
				execute(&prepared, &root, receiver, &|_| {}).await
			}
		};
		let written = run(call("write_file", json!({"path":"dir/a.txt","content":"one\ntwo\n"}))).await;
		assert!(!written.failed, "{}", written.output);
		let edited = run(call(
			"edit_file",
			json!({"path":"dir/a.txt","old_text":"two","new_text":"three"}),
		))
		.await;
		assert_eq!(edited.diff.as_deref(), Some("@@ -2,1 +2,1 @@\n-two\n+three"));
		let read = run(call("read_file", json!({"path":"dir/a.txt"}))).await;
		assert_eq!(read.output, "one\nthree");
		let listed = run(call("list_directory", json!({"path":"."}))).await;
		assert_eq!(listed.output, "dir/");
		#[cfg(unix)]
		{
			let ran = run(call("run_command", json!({"command":"cat dir/a.txt; exit 3"}))).await;
			assert!(ran.failed);
			assert_eq!(ran.exit_code, Some(3));
			assert_eq!(ran.output, "one\nthree\n[exit code 3]");
		}
	}
}
