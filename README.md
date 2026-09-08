# ReCodex

A local desktop workspace for Codex, built with Tauri 2, SvelteKit, Svelte 5, and shadcn-svelte's Rhea components. The interface uses a quiet neutral palette, compact controls, and light and dark themes.

## Features

- Streamed Markdown chat, conversation history, and generation cancellation.
- Live model catalog and per-model reasoning effort selection.
- Command and file-change activity, diffs, approval prompts, and structured user questions.
- Open, rename, switch, and remove local projects. Removing a project only removes its sidebar registration.
- Browse and edit existing UTF-8 files, save with Ctrl/Cmd+S, and detect changes made on disk before saving.
- ChatGPT sign-in through Codex and reconnect controls with connection diagnostics.

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

Choose **Open a project**, select its directory, choose a model, and send a message. Open **Files** for direct editing. A new chat uses the selected directory as its working directory with `workspace-write` and `on-request` approval settings.

`pnpm dev` runs a browser-only preview. Desktop IPC and filesystem access are intentionally unavailable in that preview; it does not generate simulated assistant responses.

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

The Playwright suite uses installed Microsoft Edge and mocked Tauri IPC to test chat streaming, approvals, model changes, file editing, project renaming, and history restoration. It does not prove native dialog or OAuth behavior. Change `channel` in `playwright.config.ts` when testing on another browser.

Live app-server checks are opt-in. `pnpm test:server` runs both checks using the project-local CLI; it requires sign-in and consumes one small model turn. The following individual Rust commands use the executable on `PATH` unless `RECODEX_CODEX_PATH` is set:

```sh
# No model usage: handshake, model catalog, and account status.
cargo test --manifest-path src-tauri/Cargo.toml --lib live_handshake_and_model_list -- --ignored
# Requires sign-in and uses one small model turn in an ephemeral conversation.
cargo test --manifest-path src-tauri/Cargo.toml --lib live_chat_turn -- --ignored
```

Biome handles frontend formatting and linting. Use `pnpm format`, `pnpm format:check`, and `pnpm lint` for frontend changes; use `cargo fmt --manifest-path src-tauri/Cargo.toml` for Rust changes.

## Architecture and scope

- `src-tauri/src/server.rs` owns the child process, initialization, concurrent request correlation, response timeouts, and event forwarding. The child terminates when the app exits.
- `src-tauri/src/workspace.rs` stores project registrations in the Tauri app-data directory and restricts editor paths to registered roots, including resolved symbolic links.
- `src/lib/app.svelte.ts` coordinates projects, account state, models, conversations, streaming events, and server requests.
- `src/lib/conversation.ts` reconciles optimistic user messages, incremental output, and completed items.
- `src/lib/components/ui` contains the generated Rhea components; `components.json` retains the registry configuration.

Conversation history and authentication remain owned by Codex. The project registry is `projects.json` in the platform's application-data directory for `com.poti.recodex`; theme, model, and last project preferences use local storage.

The basic editor handles existing UTF-8 text files up to 2 MB. It omits `.git`, `node_modules`, `target`, and `.svelte-kit` from browsing. Reload an open file to see external changes. One conversation runs at a time; project and conversation switching are disabled during a turn. Unsupported server-initiated tools return an explicit protocol error. Remote servers, full terminal emulation, attachments, and Git operations are outside this implementation.

The bridge follows the [official app-server documentation](https://developers.openai.com/codex/app-server/). Rhea is provided through the [shadcn-svelte registry](https://www.shadcn-svelte.com/docs/changelog).
