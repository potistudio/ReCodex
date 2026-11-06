//! Authorization Service: decides allow / ask / deny for a prepared call.
//!
//! Reads inside the project root are allowed. File changes and commands require approval
//! unless the user granted that approval kind for the session. There is no OS sandbox:
//! an approved command runs with the user's privileges.
use super::ledger::{kind, Event};
use super::tools::{PreparedCall, ToolKind};

pub const VERSION: &str = "default-1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
	Command,
	FileChange,
}

impl ApprovalKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Command => "command",
			Self::FileChange => "fileChange",
		}
	}
}

#[derive(Debug, PartialEq)]
pub enum Decision {
	Allow,
	Ask(ApprovalKind),
	Deny(String),
}

impl Decision {
	pub fn label(&self) -> &'static str {
		match self {
			Self::Allow => "allow",
			Self::Ask(_) => "ask",
			Self::Deny(_) => "deny",
		}
	}
}

/// Session-scoped grants, derived from the ledger so they survive restarts and are auditable.
#[derive(Clone, Debug, Default)]
pub struct Grants {
	pub command: bool,
	pub file_change: bool,
}

impl Grants {
	pub fn from_events(events: &[Event]) -> Self {
		let mut grants = Self::default();
		for event in events {
			if event.kind == kind::APPROVAL_RESOLVED && event.payload["decision"] == "acceptForSession" {
				grants.grant(match event.payload["approvalKind"].as_str() {
					Some("command") => ApprovalKind::Command,
					_ => ApprovalKind::FileChange,
				});
			}
		}
		grants
	}

	pub fn grant(&mut self, kind: ApprovalKind) {
		match kind {
			ApprovalKind::Command => self.command = true,
			ApprovalKind::FileChange => self.file_change = true,
		}
	}

	fn covers(&self, kind: ApprovalKind) -> bool {
		match kind {
			ApprovalKind::Command => self.command,
			ApprovalKind::FileChange => self.file_change,
		}
	}
}

pub fn authorize(call: &PreparedCall, grants: &Grants) -> Decision {
	let kind = match call.kind {
		ToolKind::ReadFile | ToolKind::ListDirectory => return Decision::Allow,
		ToolKind::WriteFile | ToolKind::EditFile => ApprovalKind::FileChange,
		ToolKind::RunCommand => ApprovalKind::Command,
	};
	if call.path.as_deref() == Some(".") {
		return Decision::Deny("The project root itself cannot be written".into());
	}
	if call
		.path
		.as_deref()
		.is_some_and(|path| path == ".git" || path.starts_with(".git/"))
	{
		return Decision::Deny("Changes inside .git are not allowed".into());
	}
	if grants.covers(kind) {
		Decision::Allow
	} else {
		Decision::Ask(kind)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::harness::{model::ToolCallRequest, tools::prepare};
	use serde_json::json;

	#[test]
	fn reads_are_allowed_and_changes_need_approval_or_a_grant() {
		let directory = tempfile::tempdir().unwrap();
		let prepared = |name: &str, args: serde_json::Value| {
			prepare(
				&ToolCallRequest {
					id: "c".into(),
					name: name.into(),
					arguments: args.to_string(),
				},
				directory.path(),
			)
			.unwrap()
		};
		let read = prepared("read_file", json!({"path":"a"}));
		let write = prepared("write_file", json!({"path":"a","content":""}));
		let git = prepared("write_file", json!({"path":".git/config","content":""}));
		let command = prepared("run_command", json!({"command":"ls"}));
		let mut grants = Grants::default();
		assert_eq!(authorize(&read, &grants), Decision::Allow);
		assert_eq!(authorize(&write, &grants), Decision::Ask(ApprovalKind::FileChange));
		assert!(matches!(authorize(&git, &grants), Decision::Deny(_)));
		assert_eq!(authorize(&command, &grants), Decision::Ask(ApprovalKind::Command));
		grants.grant(ApprovalKind::Command);
		assert_eq!(authorize(&command, &grants), Decision::Allow);
		assert_eq!(authorize(&write, &grants), Decision::Ask(ApprovalKind::FileChange));
	}
}
