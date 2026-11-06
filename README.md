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

```sh
pnpm install
pnpm tauri dev
```

Launch the app, open **Settings**, and choose **Sign in** under **Command Code account**. ReCodex opens the Command Code sign-in page in your browser, receives an API key on a local callback port, and stores it in the app-data directory.

To use another OpenAI-compatible API instead, set it before launching the app:

```sh
RECODEX_OPENAI_API_KEY=sk-... pnpm tauri dev
RECODEX_OPENAI_BASE_URL=http://localhost:11434/v1 pnpm tauri dev
```

[ReCodex harness](docs/recodex-harness.md#model-connection) lists every environment variable, the order in which connections are chosen, and how the Command Code sign-in and routing work.

Choose **Open a project**, select its directory, choose a model, and send a message. Open **Files** for direct editing. The agent can read files freely; file changes and commands wait for your approval unless you allow them for the session. Approved commands run with your user privileges and no sandbox. See [ReCodex harness](docs/recodex-harness.md) for the ledger, tools, policy, and recovery behavior.

`pnpm dev` runs a browser-only preview. Desktop IPC and filesystem access are intentionally unavailable in that preview; it does not generate simulated assistant responses.

## Connect PCs on a home LAN

Any PC running the desktop app can host selected projects. Other PCs on the same network find it automatically and connect with a one-time invitation, and the host PC supplies the files, model API credentials, and execution environment. [LAN hosts](docs/lan.md) gives the setup steps and describes transport, recovery, and access control.

## Development

```sh
mise run verify
```

This runs the web type check, lint, and unit tests, then Rust formatting, clippy, and tests. [Development](docs/development.md) covers the other commands, the Playwright suite, tool versions, the repository layout, and the architecture.

## Documentation

| Document                                           | Contents                                                                |
| -------------------------------------------------- | ----------------------------------------------------------------------- |
| [Development](docs/development.md)                 | Repository layout, tool versions, build and test commands, architecture |
| [ReCodex harness](docs/recodex-harness.md)         | Model connection, run loop, ledger, tools, policy, and recovery         |
| [LAN hosts](docs/lan.md)                           | Hosting setup, pairing, access boundaries, and recovery                 |
| [Harness design](docs/harness-design.md)           | Design proposal for the harness (Japanese)                              |
| [Pi harness analysis](docs/pi-harness-analysis.md) | Analysis of the Pi harness that informed the design (Japanese)          |
