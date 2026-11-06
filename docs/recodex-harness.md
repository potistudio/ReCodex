# ReCodex harness

The ReCodex harness is the app's agent runtime. It does not delegate to another agent
product: the run loop, session history, tool execution, and authorization run inside the
Rust host process (`desktop/src/harness/`). It implements the core of the contracts proposed in
[harness-design.md](harness-design.md); branches, compaction, memory, plugins, and multiple
agents are not implemented.

## Model connection

The harness sends Chat Completions requests to one connection, chosen in this order:

1. An explicit OpenAI-compatible endpoint: `RECODEX_OPENAI_BASE_URL` or
   `RECODEX_OPENAI_API_KEY`.
2. A Command Code API key, from `COMMAND_CODE_API_KEY`, ReCodex's own sign-in, or the
   Command Code CLI's `~/.commandcode/auth.json` (in that order).
3. `OPENAI_API_KEY` against `https://api.openai.com/v1`.

| Variable                    | Meaning                                                                                                                      |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `RECODEX_OPENAI_API_KEY`    | Bearer token for the OpenAI-compatible endpoint.                                                                             |
| `RECODEX_OPENAI_BASE_URL`   | API base, default `https://api.openai.com/v1`. Set it alone for a local server such as Ollama (`http://localhost:11434/v1`). |
| `COMMAND_CODE_API_KEY`      | Command Code API key; overrides any sign-in.                                                                                 |
| `OPENAI_API_KEY`            | Used only when no other connection is configured.                                                                            |
| `RECODEX_OPENAI_MODELS`     | Optional comma-separated model IDs. When unset, the catalog is read from `GET /models`.                                      |
| `RECODEX_HARNESS_MAX_STEPS` | Step limit per run, default 64.                                                                                              |

Models appear in the picker as `recodex/<model-id>`; chat IDs start with `rx-`.
`provider/list` reports the active connection and why it is unavailable. Environment variables
are read once at startup; a sign-in or sign-out switches the connection immediately, and a
step that already started keeps the connection it began with. Credentials are never written to
the ledger.

### Command Code sign-in

**Sign in** under **Command Code account** in Settings runs the same browser flow as `cmd login`
in the Command Code CLI (observed in command-code 1.88; the Studio endpoint is not publicly
documented and may change):

1. The host binds a loopback port from 127.0.0.1:5959 to 5968 and generates a 256-bit state
   token.
2. The UI opens `https://commandcode.ai/studio/auth/cli?callback=http://127.0.0.1:<port>/callback&state=<state>&mode=redirect`.
3. After the user approves, the Studio posts `apiKey`, `userId`, `userName`, `keyName`, and
   `state` to the callback, as a form (redirected to `/callback/complete`) or as JSON. A
   mismatched state is rejected, and a denial ends the attempt.
4. The key is checked with `GET https://api.commandcode.ai/alpha/whoami`. An invalid key is not
   stored.
5. The credentials are written to `command-code-auth.json` in the app-data directory with mode
   `0600` (atomic replacement), and the harness switches to the Provider API at
   `https://api.commandcode.ai/provider/v1`.

An attempt times out after five minutes, and starting a new attempt or choosing **Cancel**
closes the previous callback port. Only the host's own desktop UI can sign in or out; LAN and
Web UI clients see the state but cannot change it. **Sign out** deletes only ReCodex's file. The
issued key stays valid until it is revoked in Command Code Studio, and a key from the
environment or the CLI's file cannot be signed out from ReCodex. ReCodex reads the CLI's file
but never writes it.

### Command Code routing

`CommandCodeClient` sends each model to the wire that serves it, using the
`supported_endpoints` field of `GET /provider/v1/models`:

| Model                                      | Wire                              | Endpoint                        |
| ------------------------------------------ | --------------------------------- | ------------------------------- |
| `claude-*`, or only `/messages` advertised | Anthropic Messages                | `/provider/v1/messages`         |
| `/responses` advertised                    | OpenAI Responses (`store: false`) | `/provider/v1/responses`        |
| Everything else                            | Chat Completions                  | `/provider/v1/chat/completions` |

All three use `Authorization: Bearer <key>`; Messages also sends `anthropic-version:
2023-06-01`. Messages requests do not enable extended thinking, because continuing a tool loop
with thinking requires replaying signed thinking blocks that the ledger does not keep, so the
effort setting has no effect on Claude models. Responses requests do not replay reasoning items
between steps.

Plans without Provider API access (such as Go) get `403` with error code `upgrade_required`.
The client then switches that key to `POST https://api.commandcode.ai/alpha/generate`, the
endpoint the `cmd` CLI itself uses, for the rest of the process:

- The body carries AI SDK model messages, tools with `input_schema`, the system prompt, and a
  `config` block with the project root; the chat ID is sent as `threadId`.
- The stream is AI SDK parts (`text-delta`, `reasoning-delta`, `tool-input-*`, `tool-call`,
  `finish`, `error`) as SSE `data:` lines or bare JSON lines.
- Requests send CLI-style headers (`User-Agent: cli`, `x-command-code-version`,
  `x-cli-environment`, `x-project-slug`) and `x-taste-learning: false`.
- Tool-call IDs longer than 64 characters are shortened deterministically.

The fallback happens only for a rejection before any output, so it never duplicates text. The
generation endpoint is undocumented; its format follows the MIT-licensed
[pi-commandcode-provider](https://github.com/patlux/pi-commandcode-provider) and may change
without notice.

## Components

| Module                                                   | Role (design-document service)                                                                                     |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `ledger.rs`                                              | Session Ledger. SQLite in WAL mode with `synchronous=FULL`, stored as `harness.sqlite3` in the app-data directory. |
| `context.rs`                                             | Context Engine and Projection Engine. Builds model messages and the UI transcript from ledger events.              |
| `model.rs`                                               | Model Gateway. `ModelClient` trait, shared HTTP/SSE helpers, and the Chat Completions adapter.                     |
| `tools.rs`                                               | Tool Registry, Tool Scheduler, and the local Execution Environment.                                                |
| `policy.rs`                                              | Authorization Service with session grants.                                                                         |
| `driver.rs`                                              | Run Driver and crash recovery.                                                                                     |
| `account.rs`                                             | Model connection selection and the Command Code account.                                                           |
| `command_code.rs`                                        | Command Code browser sign-in, key validation, and credential storage.                                              |
| `command_code_client.rs`                                 | Per-model routing and the generation fallback for Command Code.                                                    |
| `messages_api.rs`, `responses_api.rs`, `generate_api.rs` | Anthropic Messages, OpenAI Responses, and Command Code generation wire adapters.                                   |
| `mod.rs`                                                 | Harness Coordinator: session and run lifecycle, approvals, catalog.                                                |

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

| Tool             | Effect              | Default policy    |
| ---------------- | ------------------- | ----------------- |
| `read_file`      | read `file:<path>`  | allow             |
| `list_directory` | read `file:<path>`  | allow             |
| `write_file`     | write `file:<path>` | ask (file change) |
| `edit_file`      | write `file:<path>` | ask (file change) |
| `run_command`    | exclusive           | ask (command)     |

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

`cargo test --manifest-path desktop/Cargo.toml --lib harness` runs the harness tests
without network access. They cover the ledger, stream parsing, path boundaries, scheduling,
policy, approval and decline flows through `Host`, interruption during a model request, crash
recovery, and the HTTP adapter against a local mock server.
