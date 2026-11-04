# ReCodex harness

The ReCodex harness is the app's agent runtime. It does not delegate to another agent
product: the run loop, session history, tool execution, and authorization run inside the
Rust host process (`src-tauri/src/harness/`). It implements the core of the contracts proposed in
[harness-design.md](harness-design.md); branches, compaction, memory, plugins, and multiple
agents are not implemented.

## Configuration

The harness talks to any OpenAI-compatible Chat Completions endpoint.

| Variable | Meaning |
| --- | --- |
| `RECODEX_OPENAI_API_KEY` | Bearer token. Falls back to `OPENAI_API_KEY`. |
| `RECODEX_OPENAI_BASE_URL` | API base, default `https://api.openai.com/v1`. Set it alone for a local server such as Ollama (`http://localhost:11434/v1`). |
| `RECODEX_OPENAI_MODELS` | Optional comma-separated model IDs. When unset, the catalog is read from `GET /models`. |
| `RECODEX_HARNESS_MAX_STEPS` | Step limit per run, default 64. |

The provider is available when an API key or a base URL is set and the session ledger opened.
`provider/list` reports the reason otherwise. Models appear in the picker as
`recodex/<model-id>`; chat IDs start with `rx-`. The credentials are read from the environment
of the host process and are never written to the ledger.

## Components

| Module | Role (design-document service) |
| --- | --- |
| `ledger.rs` | Session Ledger. SQLite in WAL mode with `synchronous=FULL`, stored as `harness.sqlite3` in the app-data directory. |
| `context.rs` | Context Engine and Projection Engine. Builds model messages and the UI transcript from ledger events. |
| `model.rs` | Model Gateway. `ModelClient` trait and the OpenAI-compatible streaming adapter. |
| `tools.rs` | Tool Registry, Tool Scheduler, and the local Execution Environment. |
| `policy.rs` | Authorization Service with session grants. |
| `driver.rs` | Run Driver and crash recovery. |
| `mod.rs` | Harness Coordinator: session and run lifecycle, approvals, catalog. |

`host.rs` exposes the harness to local, LAN, and Web UI clients, orders operations, and relays
approval answers. Events use the `thread/*`, `turn/*`, and `item/*` vocabulary the UI
consumes.

## Ledger and projections

Every fact is an event appended to the chat's stream: `session.created`, `session.model`,
`input.accepted`, `run.started`, `step.started`, `attempt.failed`, `message.assistant`,
`tool.started`, `tool.authorized`, `approval.requested`, `approval.resolved`,
`tool.dispatched`, `tool.completed`, `run.cancelRequested`, and `run.finished`. Each event has
a schema version, a per-chat sequence number, a unique ID, and a JSON payload.

- An append takes the current sequence inside an immediate transaction; callers can pass the
  sequence they observed and are rejected when the stream moved.
- The ledger keeps an exclusive SQLite lock for the life of the process. A second ReCodex
  process using the same app-data directory cannot open it, so two writers never interleave.
- Committed facts are broadcast only after the append succeeds. Streaming text and reasoning
  deltas are transient; the assistant message becomes durable when `message.assistant` is
  committed. An interrupted attempt keeps its partial text in `attempt.failed` for auditing,
  but that text is not shown to the model.
- `step.started` records the model, effort, tool set, policy version, context transformation
  version, and the last sequence used to build the request. The exact model input can be
  rebuilt from those events.
- The chat list, resumed transcripts, token usage, and session grants are projections of the
  events. Deleting a chat deletes its events.

## Runs, steps, and attempts

A prompt is recorded as `input.accepted` and starts one run (a ReCodex turn). Each step builds
the context, sends one model request, records the assistant message, and then handles the
requested tool calls. The run continues while the model requests tools and stops at the step
limit.

A model request is retried up to three times with backoff only when it failed before any
output reached the UI and the error is a transport error, HTTP 429, or a server error. Each
failed attempt is recorded.

`turn/interrupt` records `run.cancelRequested` and signals the run. The turn is reported as
`interrupted` only after the in-flight model request or command has actually stopped.

## Tools

| Tool | Effect | Default policy |
| --- | --- | --- |
| `read_file` | read `file:<path>` | allow |
| `list_directory` | read `file:<path>` | allow |
| `write_file` | write `file:<path>` | ask (file change) |
| `edit_file` | write `file:<path>` | ask (file change) |
| `run_command` | exclusive | ask (command) |

The pipeline for each call is: schema validation and path normalization, authorization,
approval when requested, revalidation, dispatch, and commit of the result.

- Paths must be relative. The nearest existing ancestor is resolved through symbolic links and
  must stay inside the project root. Writes to the root itself or inside `.git` are denied.
- An approval binds the call to a fingerprint of its normalized action. The call is prepared
  again before dispatch and fails if it no longer matches.
- All approval requests of a step are shown at once. **Allow for session** records a grant for
  that approval kind in the chat; grants are derived from the ledger and survive restarts.
- The scheduler runs calls concurrently unless they touch the same resource with a write.
  Commands have unknown effects and always run alone.
- Commands run in the project root through `/bin/sh -c` (PowerShell on Windows) with a
  default timeout of 120 seconds and a maximum of 600 seconds.
- Results shown to the model are limited to 32 KB (head and tail); the ledger keeps up to 1 MB
  of output.

There is no OS sandbox. An approved command runs with the user's privileges, and a command can
read or change files outside the project. Only approve commands you would run yourself.

## Recovery

On startup the harness closes runs that never recorded `run.finished`:

- Unanswered approvals are resolved as `expired`.
- A call that was started but never dispatched is recorded as `cancelled`.
- A call that was dispatched without a result is recorded as `outcomeUnknown`. It is never
  re-executed; the model is told the outcome is unknown and should inspect the current state.
- The run is recorded as `interrupted`.

## Limitations

- Context is sent in full on every step. There is no compaction or token budgeting, so a long
  chat eventually exceeds the model's context window and the request fails.
- Only text input is supported, and steering or queuing input during a run is not.
- The harness has no branch, memory, plugin, or multi-agent support.
- Model capabilities are not discovered. Reasoning effort is sent as `reasoning_effort` when
  it is not `off`; models that do not support the parameter reject the request.

## Tests

`cargo test --manifest-path src-tauri/Cargo.toml --lib harness` runs the harness tests
without network access. They cover the ledger, stream parsing, path boundaries, scheduling,
policy, approval and decline flows through `Host`, interruption during a model request, crash
recovery, and the HTTP adapter against a local mock server.
