# ReCodex

A local desktop workspace for ReCodex's own coding-agent harness, built with Tauri 2, Svelte 5, Vite, and shadcn-svelte's Rhea components. The interface uses compact controls and a fixed light palette.

## Features

- Streamed Markdown chat, conversation history, and generation cancellation.
- Live model catalog and per-model reasoning effort selection.
- An agent harness that runs in the app: an SQLite session ledger, tool execution in the project, and approval prompts. Models come from Command Code (browser sign-in) or any OpenAI-compatible Chat Completions API.
- Command and file-change activity, diffs, and approval prompts.
- Open, rename, switch, and remove local projects. Removing a project only removes its sidebar registration.
- Browse and edit existing UTF-8 files, save with Ctrl/Cmd+S, and detect changes made on disk before saving.
- Reconnect controls and connection diagnostics.
- Host selected projects on a home LAN and connect to other PCs over authenticated TLS WebSockets.
- Switch hosts while work continues, and send messages, edit files, stop runs, or answer approvals from any authorized device.

## Run

Install the Node.js, pnpm, and Biome versions pinned in `mise.toml` (`mise install` does it). The Rust version is pinned in `rust-toolchain.toml`, and rustup installs it on first use. You also need the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). On Windows, WebView2 and the Visual Studio C++ build tools are required.

Launch the app, open **Settings**, and choose **Sign in** under **Command Code account**. ReCodex opens the Command Code sign-in page in your browser, receives an API key on a local callback port, and stores it in the app-data directory. Plans with Provider API access use it directly; other plans, such as Go, fall back to the endpoint the Command Code CLI uses. Claude models are served through the Anthropic Messages wire.

```sh
pnpm install
pnpm tauri dev
```

To use another OpenAI-compatible API instead, set it before launching the app:

```sh
RECODEX_OPENAI_API_KEY=sk-... pnpm tauri dev
RECODEX_OPENAI_BASE_URL=http://localhost:11434/v1 pnpm tauri dev
```

| Variable | Meaning |
| --- | --- |
| `RECODEX_OPENAI_API_KEY` | Bearer token for an OpenAI-compatible endpoint; takes precedence over Command Code. |
| `RECODEX_OPENAI_BASE_URL` | API base, default `https://api.openai.com/v1`; takes precedence over Command Code. |
| `COMMAND_CODE_API_KEY` | Command Code API key; overrides the sign-in. |
| `OPENAI_API_KEY` | Used only when nothing else is configured. |
| `RECODEX_OPENAI_MODELS` | Optional comma-separated model IDs; otherwise the catalog comes from `GET /models`. |
| `RECODEX_HARNESS_MAX_STEPS` | Step limit per turn, default 64. |

Choose **Open a project**, select its directory, choose a model, and send a message. Open **Files** for direct editing. The agent can read files freely; file changes and commands wait for your approval unless you allow them for the session. Approved commands run with your user privileges and no sandbox. See [ReCodex harness](docs/recodex-harness.md) for the ledger, tools, policy, and recovery behavior.

`pnpm dev` runs a browser-only preview. Desktop IPC and filesystem access are intentionally unavailable in that preview; it does not generate simulated assistant responses.

## Connect PCs on a home LAN

Run the desktop app on each PC and sign in to Command Code (or configure another model API) on the PCs that will host projects. Open the projects you want to use on their respective PCs.

1. On the PC that owns a project, open **Connections**, select the projects to share, and choose **Start hosting**. The default listen address is `0.0.0.0:43177`.
2. Other PCs find it automatically under **Nearby PCs** using mDNS. Copy its **Connection invitation** to a PC you want to authorize through a trusted channel. Invitations expire after ten minutes and can be used once.
3. Paste the invitation into **Connect to another PC** and choose **Connect to PC**. ReCodex locates the matching host automatically; no IP address is required.
4. Registered PCs reconnect automatically on launch, after connection loss, and when discovery finds a changed address. **Active host** restores your last selection and switches between connected PCs. Repeat the initial invitation exchange in the opposite direction for mutual hosting.

The host PC supplies its files, model API credentials, and execution environment. Work continues when the controlling UI switches hosts or a client disconnects. Hosting runs while the host app is open and resumes automatically on the next launch until you choose **Stop hosting**. Registered device permissions, connection profiles, and certificate pins survive app restarts. **Disconnect and forget** removes a saved client connection and stops its retries; **Revoke** on the host invalidates that device's credential, including after host restarts. Each authorized device can operate and approve requests for all projects you share.

All authorized devices can operate on a shared project without taking or releasing control. The host orders operations, rejects a second turn in an already-running chat, accepts each approval answer once, and rejects saves based on stale file contents. Different chats in the same project can run independently. Model API configuration and project registration are performed on the host PC.

Automatic discovery uses UDP 5353 on the local link; allow local-network access and the listening TCP port on your private LAN when requested by the OS. If multicast is unavailable, **Manual address (optional)** accepts a LAN IP or hostname and port; registered connections also retry their saved addresses. `0.0.0.0` is a bind address, not a destination. See [LAN architecture and behavior](docs/lan.md) for transport, recovery, and access-control details.

## Build and verification

With [mise](https://mise.jdx.dev/), one command runs every check that needs no browser: the web type check, lint, and unit tests, then Rust formatting, clippy, and tests. `mise run verify:web` and `mise run verify:rust` run each half on its own. Playwright and the bundle build stay separate.

```sh
mise run verify
```

The individual commands are:

```sh
pnpm check
pnpm test
pnpm test:e2e
pnpm build
cargo test --manifest-path desktop/Cargo.toml
cargo clippy --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
pnpm tauri build
```

The Playwright suite uses installed Microsoft Edge and mocked Tauri IPC to test chat streaming, approvals, model changes, file editing, project renaming, and history restoration. It does not prove native dialog behavior. Set `RECODEX_TEST_BROWSER=chrome` to use installed Google Chrome, or `RECODEX_TEST_BROWSER=chromium` to use the Playwright Chromium build matching the installed Playwright version. The default channel is Microsoft Edge.

Rust tests use a scripted model and a local mock HTTP server, so they need no API key or network access.

Biome handles frontend formatting and linting. Use `pnpm format`, `pnpm format:check`, and `pnpm lint` for frontend changes; use `cargo fmt --manifest-path desktop/Cargo.toml` for Rust changes.

## Architecture and scope

- `desktop/` is the Tauri shell and Rust host: the window, IPC commands, bundle, and harness process.
- `desktop/src/host.rs` owns the transport-independent workspace, operation ordering, pending approvals, and bounded event replay.
- `desktop/src/harness/` is ReCodex's own agent harness: the SQLite session ledger, run driver, OpenAI-compatible model gateway, tool scheduler, and approval policy.
- `desktop/src/discovery.rs` browses and advertises LAN hosts using mDNS/DNS-SD; `lan_store.rs` persists trust and hosting preferences with atomic file replacement.
- `desktop/src/lan.rs` provides certificate-verified TLS WebSockets, single-use pairing invitations, persistent device credentials, revocation, and remote request correlation.
- `desktop/src/workspace.rs` stores project registrations in the supplied app-data directory and restricts editor paths to registered roots, including resolved symbolic links.
- `web/` holds the Svelte web client used by the desktop WebView, the browser Web UI, and browser preview.
- `web/src/App.svelte` is the single view host; `web/src/lib/Workspace.svelte` holds the workspace UI.
- `web/src/lib/app.svelte.ts` coordinates projects, providers, models, conversations, streaming events, and server requests.
- `web/src/lib/conversation.ts` reconciles optimistic user messages, incremental output, and completed items.
- `web/src/lib/components/ui` contains the generated Rhea components; `components.json` retains the registry configuration.

Chats are stored in `harness.sqlite3` and a Command Code sign-in in `command-code-auth.json` (mode `0600`), both in the platform's application-data directory for `com.poti.recodex`. Other API credentials are read from the environment and never stored. The project registry is `projects.json` in the platform's application-data directory for `com.poti.recodex`; model and last project preferences use local storage.

The basic editor handles existing UTF-8 text files up to 2 MB. It omits `.git`, `node_modules`, `target`, and `build` from browsing. Reload an open file to see external changes. Each host view keeps independent conversation state. Switching between hosts keeps their views and running work. LAN hosts are discovered automatically and require the host desktop app to remain open. Full terminal emulation, attachments, and Git operations are outside this implementation.

Rhea is provided through the [shadcn-svelte registry](https://www.shadcn-svelte.com/docs/changelog).
