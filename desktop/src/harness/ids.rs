//! Typed identifiers for the harness domain.
//!
//! Each identifier wraps a UUID in its own type, so a chat ID cannot be passed where a run ID
//! is expected. Text exists only at the edges: JSON payloads, the ledger's `TEXT` columns, and
//! outbound provider requests. Every text form is canonical (lowercase, hyphenated), so one
//! identifier has exactly one spelling and text equality in SQL matches identifier equality.
use rusqlite::{
	types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef},
	ToSql,
};
use serde::{Serialize, Serializer};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// The text did not spell an identifier of the expected type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidId(&'static str);

impl fmt::Display for InvalidId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Not a valid {} ID", self.0)
	}
}

impl std::error::Error for InvalidId {}

/// Parse the canonical form of a UUID. `Uuid::parse_str` also accepts uppercase, braced, and
/// unhyphenated spellings, which would give one identifier several texts.
fn canonical_uuid(text: &str) -> Option<Uuid> {
	let uuid = Uuid::parse_str(text).ok()?;
	(uuid.hyphenated().to_string() == text).then_some(uuid)
}

macro_rules! id_type {
	($(#[$meta:meta])* $name:ident, $label:literal, $prefix:literal) => {
		$(#[$meta])*
		#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
		pub struct $name(Uuid);

		impl $name {
			pub fn new() -> Self {
				Self(Uuid::new_v4())
			}
		}

		impl Default for $name {
			fn default() -> Self {
				Self::new()
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				write!(f, "{}{}", $prefix, self.0.hyphenated())
			}
		}

		impl FromStr for $name {
			type Err = InvalidId;

			fn from_str(text: &str) -> Result<Self, InvalidId> {
				text.strip_prefix($prefix)
					.and_then(canonical_uuid)
					.map(Self)
					.ok_or(InvalidId($label))
			}
		}

		impl Serialize for $name {
			fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
				serializer.collect_str(self)
			}
		}

		impl ToSql for $name {
			fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
				Ok(ToSqlOutput::from(self.to_string()))
			}
		}

		impl FromSql for $name {
			fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
				value
					.as_str()?
					.parse()
					.map_err(|error: InvalidId| FromSqlError::Other(Box::new(error)))
			}
		}
	};
}

id_type!(
	/// A chat. Its text form is `rx-<uuid>`, the spelling already stored in the ledger and
	/// used by the UI.
	ThreadId,
	"chat",
	"rx-"
);

impl ThreadId {
	/// The UUID without the `rx-` prefix, which is how providers outside the harness know the
	/// chat.
	pub fn uuid(&self) -> Uuid {
		self.0
	}
}

id_type!(
	/// One run of the agent in a chat. The UI calls it a turn ID.
	RunId,
	"turn",
	""
);

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn text_form_round_trips() {
		let thread = ThreadId::new();
		assert_eq!(thread.to_string().parse::<ThreadId>(), Ok(thread));
		assert!(thread.to_string().starts_with("rx-"));
		let run = RunId::new();
		assert_eq!(run.to_string().parse::<RunId>(), Ok(run));
		assert_eq!(thread.to_string(), format!("rx-{}", thread.uuid()));
	}

	#[test]
	fn text_that_is_not_the_canonical_spelling_is_rejected() {
		let uuid = "67e55044-10b1-426f-9247-bb680e5fe0c8";
		assert!(format!("rx-{uuid}").parse::<ThreadId>().is_ok());
		for text in [
			"",
			"rx-",
			"rx-1",
			"unknown-thread",
			uuid,
			&format!("RX-{uuid}"),
			&format!("rx-{}", uuid.to_uppercase()),
			&format!("rx-{}", uuid.replace('-', "")),
			&format!("rx-{{{uuid}}}"),
			&format!("rx-rx-{uuid}"),
			&format!("rx-{uuid} "),
		] {
			assert!(
				text.parse::<ThreadId>().is_err(),
				"{text:?} must not parse as a chat ID"
			);
		}
		assert!(format!("rx-{uuid}").parse::<RunId>().is_err());
		assert!(uuid.parse::<RunId>().is_ok());
		assert!(uuid.to_uppercase().parse::<RunId>().is_err());
	}

	#[test]
	fn identifiers_serialize_as_their_text_form() {
		let thread = ThreadId::new();
		let run = RunId::new();
		assert_eq!(
			json!({"t":thread,"r":run}),
			json!({"t":thread.to_string(),"r":run.to_string()})
		);
	}

	#[test]
	fn identifiers_are_stored_and_read_as_text() {
		let connection = rusqlite::Connection::open_in_memory().unwrap();
		connection.execute_batch("CREATE TABLE t (id TEXT NOT NULL)").unwrap();
		let thread = ThreadId::new();
		connection.execute("INSERT INTO t VALUES (?1)", [&thread]).unwrap();
		let text: String = connection.query_row("SELECT id FROM t", [], |row| row.get(0)).unwrap();
		assert_eq!(text, thread.to_string());
		let read: ThreadId = connection.query_row("SELECT id FROM t", [], |row| row.get(0)).unwrap();
		assert_eq!(read, thread);
		connection.execute("UPDATE t SET id = 'rx-1'", []).unwrap();
		assert!(connection
			.query_row("SELECT id FROM t", [], |row| row.get::<_, ThreadId>(0))
			.is_err());
	}
}
