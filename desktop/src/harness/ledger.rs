//! Session Ledger: the authoritative, append-only event log of harness sessions.
//!
//! Every fact the harness relies on (accepted input, model requests, assistant messages,
//! authorization decisions, tool receipts, run outcomes) is committed here before it is
//! broadcast. Transcripts, model context, and usage are projections of these events.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use std::{fs, path::Path, sync::Mutex};
use uuid::Uuid;

/// Version of the event envelope written by this build.
pub const SCHEMA: i64 = 1;

pub mod kind {
	pub const SESSION_CREATED: &str = "session.created";
	pub const SESSION_MODEL: &str = "session.model";
	pub const INPUT_ACCEPTED: &str = "input.accepted";
	pub const RUN_STARTED: &str = "run.started";
	pub const STEP_STARTED: &str = "step.started";
	pub const ATTEMPT_FAILED: &str = "attempt.failed";
	pub const MESSAGE_ASSISTANT: &str = "message.assistant";
	pub const TOOL_STARTED: &str = "tool.started";
	pub const TOOL_AUTHORIZED: &str = "tool.authorized";
	pub const APPROVAL_REQUESTED: &str = "approval.requested";
	pub const APPROVAL_RESOLVED: &str = "approval.resolved";
	pub const TOOL_DISPATCHED: &str = "tool.dispatched";
	pub const TOOL_COMPLETED: &str = "tool.completed";
	pub const RUN_CANCEL_REQUESTED: &str = "run.cancelRequested";
	pub const RUN_FINISHED: &str = "run.finished";
}

#[derive(Clone, Debug)]
pub struct Event {
	pub seq: u64,
	pub kind: String,
	pub payload: Value,
	pub at: u64,
}

#[derive(Clone, Debug)]
pub struct SessionRow {
	pub id: String,
	pub cwd: String,
	pub name: Option<String>,
	pub model: Option<String>,
	pub effort: Option<String>,
	pub preview: String,
	pub updated_at: u64,
}

pub struct Ledger {
	connection: Mutex<Connection>,
}

fn error(e: impl std::fmt::Display) -> String {
	format!("Session ledger: {e}")
}

fn now_secs() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|duration| duration.as_secs())
		.unwrap_or_default()
}

impl Ledger {
	/// Open the ledger and take the exclusive writer lock. A second process using the same
	/// file fails here instead of interleaving writes with the current owner.
	pub fn open(path: &Path) -> Result<Self, String> {
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(error)?;
		}
		let connection = Connection::open(path).map_err(error)?;
		connection
			.execute_batch(
				"PRAGMA locking_mode=EXCLUSIVE;
                 PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 BEGIN EXCLUSIVE;
                 CREATE TABLE IF NOT EXISTS sessions (
                     id TEXT PRIMARY KEY,
                     cwd TEXT NOT NULL,
                     name TEXT,
                     model TEXT,
                     effort TEXT,
                     preview TEXT NOT NULL DEFAULT '',
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL,
                     last_seq INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS events (
                     session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                     seq INTEGER NOT NULL,
                     id TEXT NOT NULL UNIQUE,
                     schema INTEGER NOT NULL,
                     kind TEXT NOT NULL,
                     payload TEXT NOT NULL,
                     at INTEGER NOT NULL,
                     PRIMARY KEY (session_id, seq)
                 );
                 CREATE INDEX IF NOT EXISTS events_kind ON events(kind);
                 CREATE INDEX IF NOT EXISTS sessions_cwd ON sessions(cwd);
                 COMMIT;",
			)
			.map_err(|e| {
				if matches!(
					e.sqlite_error_code(),
					Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
				) {
					"Session ledger is in use by another ReCodex process".to_owned()
				} else {
					error(e)
				}
			})?;
		Ok(Self {
			connection: Mutex::new(connection),
		})
	}

	pub fn create_session(
		&self,
		id: &str,
		cwd: &str,
		model: Option<&str>,
		effort: Option<&str>,
		payload: Value,
	) -> Result<Event, String> {
		let mut connection = self.connection.lock().unwrap();
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(error)?;
		let at = now_secs();
		transaction
			.execute(
				"INSERT INTO sessions (id, cwd, model, effort, created_at, updated_at, last_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0)",
				params![id, cwd, model, effort, at as i64],
			)
			.map_err(error)?;
		let event = insert(&transaction, id, 0, kind::SESSION_CREATED, payload, at)?;
		transaction.commit().map_err(error)?;
		Ok(event)
	}

	/// Append one event. `expected` is the last sequence the writer observed; a different
	/// current sequence means another writer advanced the stream and the append is rejected.
	pub fn append(&self, session: &str, expected: Option<u64>, kind: &str, payload: Value) -> Result<Event, String> {
		let mut connection = self.connection.lock().unwrap();
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(error)?;
		let last: Option<i64> = transaction
			.query_row("SELECT last_seq FROM sessions WHERE id = ?1", params![session], |row| {
				row.get(0)
			})
			.optional()
			.map_err(error)?;
		let last = last.ok_or("This chat no longer exists")? as u64;
		if expected.is_some_and(|expected| expected != last) {
			return Err("This chat changed while the operation was being accepted".into());
		}
		let at = now_secs();
		match kind {
			kind::INPUT_ACCEPTED => {
				let preview: String = payload["text"].as_str().unwrap_or_default().chars().take(200).collect();
				transaction
					.execute(
						"UPDATE sessions SET preview = ?2 WHERE id = ?1 AND preview = ''",
						params![session, preview],
					)
					.map_err(error)?;
			}
			kind::SESSION_MODEL => {
				transaction
					.execute(
						"UPDATE sessions SET model = ?2, effort = ?3 WHERE id = ?1",
						params![session, payload["model"].as_str(), payload["effort"].as_str()],
					)
					.map_err(error)?;
			}
			_ => {}
		}
		let event = insert(&transaction, session, last, kind, payload, at)?;
		transaction.commit().map_err(error)?;
		Ok(event)
	}

	pub fn events(&self, session: &str) -> Result<Vec<Event>, String> {
		let connection = self.connection.lock().unwrap();
		let mut statement = connection
			.prepare_cached("SELECT seq, kind, payload, at FROM events WHERE session_id = ?1 ORDER BY seq")
			.map_err(error)?;
		let rows = statement
			.query_map(params![session], |row| {
				Ok((
					row.get::<_, i64>(0)?,
					row.get::<_, String>(1)?,
					row.get::<_, String>(2)?,
					row.get::<_, i64>(3)?,
				))
			})
			.map_err(error)?;
		let mut events = Vec::new();
		for row in rows {
			let (seq, kind, payload, at) = row.map_err(error)?;
			events.push(Event {
				seq: seq as u64,
				kind,
				payload: serde_json::from_str(&payload).map_err(error)?,
				at: at as u64,
			});
		}
		Ok(events)
	}

	pub fn session(&self, id: &str) -> Result<Option<SessionRow>, String> {
		let connection = self.connection.lock().unwrap();
		connection
			.query_row(
				"SELECT id, cwd, name, model, effort, preview, updated_at FROM sessions WHERE id = ?1",
				params![id],
				session_row,
			)
			.optional()
			.map_err(error)
	}

	pub fn sessions(&self, cwd: &str) -> Result<Vec<SessionRow>, String> {
		let connection = self.connection.lock().unwrap();
		let mut statement = connection
			.prepare_cached(
				"SELECT id, cwd, name, model, effort, preview, updated_at FROM sessions
                 WHERE cwd = ?1 ORDER BY updated_at DESC",
			)
			.map_err(error)?;
		let rows = statement.query_map(params![cwd], session_row).map_err(error)?;
		rows.collect::<Result<_, _>>().map_err(error)
	}

	/// Runs that started but never recorded an outcome, as (session, runId) pairs.
	pub fn unfinished_runs(&self) -> Result<Vec<(String, String)>, String> {
		let connection = self.connection.lock().unwrap();
		let mut statement = connection
			.prepare(
				"SELECT started.session_id, json_extract(started.payload, '$.runId')
                 FROM events AS started
                 WHERE started.kind = ?1 AND NOT EXISTS (
                     SELECT 1 FROM events AS finished
                     WHERE finished.session_id = started.session_id
                       AND finished.kind = ?2
                       AND json_extract(finished.payload, '$.runId')
                           = json_extract(started.payload, '$.runId')
                 )",
			)
			.map_err(error)?;
		let rows = statement
			.query_map(params![kind::RUN_STARTED, kind::RUN_FINISHED], |row| {
				Ok((row.get(0)?, row.get(1)?))
			})
			.map_err(error)?;
		rows.collect::<Result<_, _>>().map_err(error)
	}

	pub fn delete(&self, id: &str) -> Result<(), String> {
		self.connection
			.lock()
			.unwrap()
			.execute("DELETE FROM sessions WHERE id = ?1", params![id])
			.map_err(error)?;
		Ok(())
	}
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
	Ok(SessionRow {
		id: row.get(0)?,
		cwd: row.get(1)?,
		name: row.get(2)?,
		model: row.get(3)?,
		effort: row.get(4)?,
		preview: row.get(5)?,
		updated_at: row.get::<_, i64>(6)? as u64,
	})
}

fn insert(
	transaction: &rusqlite::Transaction<'_>,
	session: &str,
	last: u64,
	kind: &str,
	payload: Value,
	at: u64,
) -> Result<Event, String> {
	let seq = last + 1;
	transaction
		.execute(
			"INSERT INTO events (session_id, seq, id, schema, kind, payload, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
			params![
				session,
				seq as i64,
				Uuid::new_v4().to_string(),
				SCHEMA,
				kind,
				payload.to_string(),
				at as i64
			],
		)
		.map_err(error)?;
	transaction
		.execute(
			"UPDATE sessions SET last_seq = ?2, updated_at = ?3 WHERE id = ?1",
			params![session, seq as i64, at as i64],
		)
		.map_err(error)?;
	Ok(Event {
		seq,
		kind: kind.to_owned(),
		payload,
		at,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn appends_are_ordered_fenced_and_survive_reopen() {
		let directory = tempfile::tempdir().unwrap();
		let path = directory.path().join("ledger.sqlite3");
		{
			let ledger = Ledger::open(&path).unwrap();
			assert!(Ledger::open(&path).is_err(), "a second writer must be fenced");
			ledger
				.create_session("rx-1", "/project", Some("m"), None, json!({}))
				.unwrap();
			let input = ledger
				.append("rx-1", Some(1), kind::INPUT_ACCEPTED, json!({"text":"hello"}))
				.unwrap();
			assert_eq!(input.seq, 2);
			assert!(ledger
				.append("rx-1", Some(1), kind::RUN_STARTED, json!({"runId":"r"}))
				.is_err());
			ledger
				.append("rx-1", None, kind::RUN_STARTED, json!({"runId":"r"}))
				.unwrap();
			assert_eq!(
				ledger.unfinished_runs().unwrap(),
				vec![("rx-1".to_owned(), "r".to_owned())]
			);
		}
		let ledger = Ledger::open(&path).unwrap();
		let events = ledger.events("rx-1").unwrap();
		assert_eq!(
			events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
			[kind::SESSION_CREATED, kind::INPUT_ACCEPTED, kind::RUN_STARTED]
		);
		assert_eq!(ledger.sessions("/project").unwrap()[0].preview, "hello");
		ledger.delete("rx-1").unwrap();
		assert!(ledger.events("rx-1").unwrap().is_empty());
		assert!(ledger.session("rx-1").unwrap().is_none());
	}
}
