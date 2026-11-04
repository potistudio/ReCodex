# ReCodex

A local desktop workspace for Codex, built with Tauri 2, SvelteKit, Svelte 5, and shadcn-svelte's Rhea components. The interface uses compact controls and a fixed light palette.

## Features

- Streamed Markdown chat, conversation history, and generation cancellation.
- Live model catalog and per-model reasoning effort selection.
- Multiple model providers in one catalog: the Codex app-server adapter, a Pi RPC adapter, and ReCodex's own harness for OpenAI-compatible APIs. Provider-specific models are selected from the same picker.
- Command and file-change activity, diffs, approval prompts, and structured user questions.
- Open, rename, switch, and remove local projects. Removing a project only removes its sidebar registration.
- Browse and edit existing UTF-8 files, save with Ctrl/Cmd+S, and detect changes made on disk before saving.
- ChatGPT sign-in through Codex and reconnect controls with connection diagnostics.
- Host selected projects on a home LAN and connect to other PCs over authenticated TLS WebSockets.
- Switch hosts while work continues, and send messages, edit files, stop runs, or answer approvals from any authorized device.

## Run

Install Node.js, pnpm, Rust stable, and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). On Windows, WebView2 and the Visual Studio C++ build tools are required. A project-local [Codex CLI](https://developers.openai.com/codex/cli/) is pinned in the development dependencies.

```sh
pnpm install
pnpm exec codex login
pnpm tauri dev
```

The app launches `codex app-server --listen stdio://` automatically. It uses the CLI's existing configuration and sign-in. `pnpm tauri` resolves the native executable from the pinned package, so a globally installed older CLI does not override the development runtime. To choose a specific executable, set `RECODEX_CODEX_PATH` before launching the app:

```powershell
$env:RECODEX_CODEX_PATH = 'C:\path\to\codex.exe'
pnpm tauri dev
```

Pi models are available when the [Pi CLI](https://pi.dev) is installed and configured with its own sign-in or provider API keys. ReCodex starts `pi --mode rpc` lazily for each Pi chat. Set `RECODEX_PI_PATH` when `pi` is not on `PATH`. Pi credentials and model configuration stay in Pi's own agent directory; ReCodex does not copy them.

ReCodex harness models are available when `RECODEX_OPENAI_API_KEY` (or `OPENAI_API_KEY`) is set, or when `RECODEX_OPENAI_BASE_URL` points at a local OpenAI-compatible server. The harness runs the agent loop, tools, and approvals in the host and stores its chats in an SQLite ledger under the app-data directory. Its commands run without a sandbox after you approve them. See [ReCodex harness](docs/recodex-harness.md).

```sh
RECODEX_OPENAI_BASE_URL=http://localhost:11434/v1 pnpm tauri dev
```

```powershell
$env:RECODEX_PI_PATH = 'C:\path\to\pi.cmd'
pnpm tauri dev
```

Choose **Open a project**, select its directory, choose a Codex or Pi model, and send a message. Open **Files** for direct editing. A Codex chat uses `workspace-write` and `on-request` approval settings. Pi runs with its own tools and configuration; ReCodex does not simulate Codex approval prompts for Pi chats. See [Pi provider adapter](docs/pi-provider.md) for the event mapping, persistence, and capability differences.

`pnpm dev` runs a browser-only preview. Desktop IPC and filesystem access are intentionally unavailable in that preview; it does not generate simulated assistant responses.

## Connect PCs on a home LAN

Run the desktop app and sign in to Codex on each PC. Open the projects you want to use on their respective PCs.

1. On the PC that owns a project, open **Connections**, select the projects to share, and choose **Start hosting**. The default listen address is `0.0.0.0:43177`.
2. Other PCs find it automatically under **Nearby PCs** using mDNS. Copy its **Connection invitation** to a PC you want to authorize through a trusted channel. Invitations expire after ten minutes and can be used once.
3. Paste the invitation into **Connect to another PC** and choose **Connect to PC**. ReCodex locates the matching host automatically; no IP address is required.
4. Registered PCs reconnect automatically on launch, after connection loss, and when discovery finds a changed address. **Active host** restores your last selection and switches between connected PCs. Repeat the initial invitation exchange in the opposite direction for mutual hosting.

The host PC supplies its files, CLI, Codex account, and execution environment. Work continues when the controlling UI switches hosts or a client disconnects. Hosting runs while the host app is open and resumes automatically on the next launch until you choose **Stop hosting**. Registered device permissions, connection profiles, and certificate pins survive app restarts. **Disconnect and forget** removes a saved client connection and stops its retries; **Revoke** on the host invalidates that device's credential, including after host restarts. Each authorized device can operate and approve requests for all projects you share.

All authorized devices can operate on a shared project without taking or releasing control. The host orders operations, rejects a second turn in an already-running chat, accepts each approval answer once, and rejects saves based on stale file contents. Different chats in the same project can run independently. Codex sign-in and project registration are performed on the host PC.

Automatic discovery uses UDP 5353 on the local link; allow local-network access and the listening TCP port on your private LAN when requested by the OS. If multicast is unavailable, **Manual address (optional)** accepts a LAN IP or hostname and port; registered connections also retry their saved addresses. `0.0.0.0` is a bind address, not a destination. See [LAN architecture and behavior](docs/lan.md) for transport, recovery, and access-control details.

## Build and verification

```sh
pnpm check
pnpm test
pnpm test:e2e
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
pnpm tauri build
```

Built applications use the Codex executable on `PATH`, or `RECODEX_CODEX_PATH` when set; the CLI is not embedded in the installer. Use a CLI version compatible with the selected model. For example, the global CLI 0.149.1 on the development machine rejected `gpt-6-astra` with a minimum-version error, so the development launcher pins CLI 0.153.4.

The Playwright suite uses installed Microsoft Edge and mocked Tauri IPC to test chat streaming, approvals, model changes, file editing, project renaming, and history restoration. It does not prove native dialog or OAuth behavior. Set `RECODEX_TEST_BROWSER=chrome` to use installed Google Chrome, or `RECODEX_TEST_BROWSER=chromium` to use the Playwright Chromium build matching the installed Playwright version. The default channel is Microsoft Edge.

Live app-server checks are opt-in. `pnpm test:server` runs both checks using the project-local CLI; it requires sign-in and consumes one small model turn. The following individual Rust commands use the executable on `PATH` unless `RECODEX_CODEX_PATH` is set:

```sh
# No model usage: handshake, model catalog, and account status.
cargo test --manifest-path src-tauri/Cargo.toml --lib live_handshake_and_model_list -- --ignored
# Requires sign-in and uses one small model turn in an ephemeral conversation.
cargo test --manifest-path src-tauri/Cargo.toml --lib live_chat_turn -- --ignored
```

Biome handles frontend formatting and linting. Use `pnpm format`, `pnpm format:check`, and `pnpm lint` for frontend changes; use `cargo fmt --manifest-path src-tauri/Cargo.toml` for Rust changes.

## Architecture and scope

- `src-tauri/src/host.rs` owns the transport-independent workspace, provider routing, operation ordering, Pi approval state, and bounded event replay.
- `src-tauri/src/harness/` is ReCodex's own agent harness: the SQLite session ledger, run driver, OpenAI-compatible model gateway, tool scheduler, and approval policy.
- `src-tauri/src/pi.rs` runs Pi in RPC mode, correlates JSONL commands, persists Pi sessions, and translates Pi events into the Codex-compatible event stream used by the UI.
- `src-tauri/src/discovery.rs` browses and advertises LAN hosts using mDNS/DNS-SD; `lan_store.rs` persists trust and hosting preferences with atomic file replacement.
- `src-tauri/src/lan.rs` provides certificate-verified TLS WebSockets, single-use pairing invitations, persistent device credentials, revocation, and remote request correlation.
- `src-tauri/src/server.rs` owns the Codex child process, initialization, concurrent request correlation, response timeouts, and event forwarding. The child terminates when the app exits.
- `src-tauri/src/workspace.rs` stores project registrations in the supplied app-data directory and restricts editor paths to registered roots, including resolved symbolic links.
- `src/lib/app.svelte.ts` coordinates projects, account state, models, conversations, streaming events, and server requests.
- `src/lib/conversation.ts` reconciles optimistic user messages, incremental output, and completed items.
- `src/lib/components/ui` contains the generated Rhea components; `components.json` retains the registry configuration.

Conversation history and authentication remain owned by each provider. Codex chats and ChatGPT sign-in stay in Codex; Pi chats, model credentials, and provider configuration stay in Pi's agent directory except for ReCodex-owned session files under the app-data directory. ReCodex harness chats are owned by ReCodex in `harness.sqlite3` under the app-data directory. The project registry is `projects.json` in the platform's application-data directory for `com.poti.recodex`; model and last project preferences use local storage.

The basic editor handles existing UTF-8 text files up to 2 MB. It omits `.git`, `node_modules`, `target`, and `.svelte-kit` from browsing. Reload an open file to see external changes. Each host view keeps independent conversation state. Switching between hosts keeps their views and running work. Unsupported server-initiated tools return an explicit protocol error. LAN hosts are discovered automatically and require the host desktop app to remain open. Full terminal emulation, attachments, and Git operations are outside this implementation. Pi chats do not expose Codex approvals, sandbox guarantees, or structured user questions; those capabilities are Codex-only. Pi extensions run in the Pi process with the user's privileges, so only use the Pi provider with projects and Pi configurations you trust.

The bridge follows the [official app-server documentation](https://developers.openai.com/codex/app-server/). Pi integration follows the [Pi RPC protocol](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md). Rhea is provided through the [shadcn-svelte registry](https://www.shadcn-svelte.com/docs/changelog).
