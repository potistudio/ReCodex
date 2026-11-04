# Pi provider adapter

ReCodex can use more than one model provider. Codex remains available through
`codex app-server`, and Pi is available through its JSONL RPC mode. The host
routes each chat to the provider that created it and exposes both model catalogs
in the existing model picker.

## Model and thread identity

Pi models are namespaced as `pi/<provider>/<model-id>`. The prefix prevents
collisions with Codex model IDs and makes provider selection derivable from the
model value alone. Each catalog entry also carries:

- `provider`: `codex` or `pi`.
- `providerLabel`: a picker label such as `Pi · anthropic`.
- `providerId`: the native Pi provider ID.
- `providerModel`: the native Pi model ID.

A Pi thread ID starts with `pi-`. The host uses that prefix to route
`thread/read`, `thread/resume`, `turn/start`, `turn/interrupt`, and
`thread/delete` to the Pi adapter. Codex thread IDs are left unchanged.

Starting a chat with a Codex model while it belongs to a Pi thread is rejected.
The UI asks the user to start a new chat instead of silently changing the
provider or mixing two histories.

## Process and session lifecycle

`src-tauri/src/pi.rs` starts one Pi child process per active Pi chat:

```text
pi --mode rpc --session-id pi-<uuid> --session-dir <app-data>/pi-sessions
```

`--session-id` makes Pi create or resume the matching session. ReCodex stores
Pi sessions under its own application-data directory, not in the project, so
deleting a Pi chat removes only the ReCodex-owned session file. The active Pi
model and thinking level are applied with `set_model` and
`set_thinking_level` before a prompt is sent.

The adapter resolves the executable in this order:

1. `RECODEX_PI_PATH`.
2. `pi.exe`, `pi.cmd`, `pi.bat`, or `pi` on `PATH` (Windows order).
3. An error that is surfaced through `provider/list` and the settings dialog.

JavaScript entry points and Windows `.cmd`/`.bat` shims are wrapped with the
appropriate launcher. A missing Pi CLI does not prevent Codex from starting.

Project-local Pi files are ignored by default (`--no-approve`). Set
`RECODEX_PI_TRUST_PROJECTS=1` to pass `--approve` and load project-local Pi
extensions, skills, and prompt templates. This is a trust decision: Pi
extensions execute in the Pi process with the user's privileges.

## Event translation

Pi emits session events such as `message_update`, `tool_execution_*`, and
`agent_settled`. The adapter converts them into the event vocabulary the
ReCodex UI already consumes:

| Pi event | ReCodex events |
| --- | --- |
| `agent_start` / `turn_start` | `turn/started` (published by the adapter before the prompt) |
| `message_update` text delta | `item/agentMessage/delta` |
| `message_update` thinking delta | `item/reasoning/summaryTextDelta` |
| `message_end` assistant | `item/completed` (`agentMessage`) and `thread/tokenUsage/updated` |
| `tool_execution_start` | `item/started` (`commandExecution`, `fileChange`, or `mcpToolCall`) |
| `tool_execution_update` | `item/updated` |
| `tool_execution_end` | `item/completed` |
| `agent_settled` | `turn/completed` |

`agent_end` is not treated as completion. Pi can still retry, compact, or run
queued work, so the ReCodex turn closes on `agent_settled`. An abort closes the
turn as `interrupted`; an assistant error closes it as `failed` with the error
message attached.

Pi tool results are mapped as follows:

- `bash` and `powershell` become command activity with streamed output.
- `edit` and `write` become file-change activity. Pi's diff or patch details are
  used when present; otherwise a bounded fallback diff is generated.
- Other built-in, extension, and MCP tools become generic tool activity.

Pi does not provide ReCodex's approval protocol. Pi chats therefore never emit
approval prompts. Running a tool is controlled by Pi's own configuration and
extension behavior, not by the ReCodex approval authority.

## Catalog and history

`get_available_models` is read from a short-lived Pi RPC process and cached for
60 seconds. The process runs with `--no-session --no-tools`; it does not create
a chat or execute tools. The catalog is merged with the Codex catalog on the
first page of `model/list`. Pagination cursors still belong to Codex.

`thread/list` merges Codex's catalog with Pi session metadata. Pi session files
are scanned for their header, display name, first user message, last model
change, and modification time. The model list and history are read through
`get_messages` when a Pi chat is resumed, so the active branch is authoritative.

## Capability differences

| Capability | Codex provider | Pi provider |
| --- | --- | --- |
| Approval prompts | Yes | No |
| `workspace-write` sandbox | Yes | No; Pi runs with its own process permissions |
| Structured user questions | Yes | No |
| Session storage | Codex-owned | Pi-owned JSONL under ReCodex app data |
| Authentication | ChatGPT through Codex | Pi `/login`, provider API keys, or ambient credentials |
| Token usage | Codex events | Pi assistant usage and session stats |

Provider availability is reported by `provider/list` and included in
`account/read`. If Codex disconnects while Pi is available, Codex turns fail
with an explicit error but the host remains connected for Pi chats.

## Tests

The adapter is tested without network access through
`src-tauri/tests/fixtures/pi.mjs`, a small RPC fixture that exercises the model
catalog, streaming text, tool activity, usage, and session history. The host
test `pi_models_and_threads_are_routed_without_codex` verifies provider
routing, event translation, resume, and provider reporting with Codex absent.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib
```
