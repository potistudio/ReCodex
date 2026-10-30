# ReCodex

A local desktop workspace for Codex, built with Tauri 2, SvelteKit, Svelte 5, and shadcn-svelte's Rhea components. The interface uses compact controls and a choice of 22 light and dark themes.

## Features

- Streamed Markdown chat, conversation history, and generation cancellation.
- Live model catalog and per-model reasoning effort selection.
- Command and file-change activity, diffs, approval prompts, and structured user questions.
- Open, rename, switch, and remove local projects. Removing a project only removes its sidebar registration.
- Browse and edit existing UTF-8 files, save with Ctrl/Cmd+S, and detect changes made on disk before saving.
- ChatGPT sign-in through Codex and reconnect controls with connection diagnostics.
- Theme previews with instant switching and a saved appearance preference.
- Host selected projects on a home LAN and connect to other PCs over authenticated TLS WebSockets.
- Switch hosts while work continues, edit remote files, answer approvals, and release project control to another device.

## Themes

Open the account button at the bottom of the sidebar, then choose a preview under **Settings → Appearance**. The palette applies across the workspace, dialogs, menus, and file editor and is restored on the next launch. Existing light and dark preferences remain supported.

Available palettes: ReCodex Light/Dark, Linear Light/Dark, OpenAI Light/Dark, Catppuccin Latte/Frappé/Macchiato/Mocha, One Dark/Light, Tokyo Night/Storm/Moon/Day, Nord, Dracula, Gruvbox Dark/Light, and Rosé Pine/Dawn.

Linear and OpenAI are inspired palettes adapted for this interface. Editor palettes use the published colors from [Catppuccin](https://catppuccin.com/palette/), [One Dark](https://github.com/atom/one-dark-syntax), [One Light](https://github.com/atom/one-light-syntax), [Tokyo Night](https://github.com/folke/tokyonight.nvim), [Nord](https://www.nordtheme.com/docs/colors-and-palettes/), [Dracula](https://draculatheme.com/contribute), [Gruvbox](https://github.com/morhetz/gruvbox), and [Rosé Pine](https://rosepinetheme.com/palette/), with surface and text adjustments for this UI.

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

## Connect PCs on a home LAN

Run the desktop app and sign in to Codex on each PC. Open the projects you want to use on their respective PCs.

1. On the PC that owns a project, open **Connections**, select the projects to share, and choose **Start hosting**. The default listen address is `0.0.0.0:43177`.
2. Other PCs find it automatically under **Nearby PCs** using mDNS. Copy its **Connection invitation** to a PC you want to authorize through a trusted channel. Invitations expire after ten minutes and can be used once.
3. Paste the invitation into **Connect to another PC** and choose **Connect to PC**. ReCodex locates the matching host automatically; no IP address is required.
4. Registered PCs reconnect automatically on launch, after connection loss, and when discovery finds a changed address. **Active host** restores your last selection and switches between connected PCs. Repeat the initial invitation exchange in the opposite direction for mutual hosting.

The host PC supplies its files, CLI, Codex account, and execution environment. Work continues when the controlling UI switches hosts or a client disconnects. Hosting runs while the host app is open and resumes automatically on the next launch until you choose **Stop hosting**. Registered device permissions, connection profiles, and certificate pins survive app restarts. **Disconnect and forget** removes a saved client connection and stops its retries; **Revoke** on the host invalidates that device's credential, including after host restarts. Each authorized device can operate and approve requests for all projects you share.

One device at a time can change each project, including through the local editor. Choose **Take control** in Connections to explicitly take over the selected project, or **Release control** on the current device to hand it over. Reads remain available to other paired devices. Codex sign-in and project registration are performed on the host PC.

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

- `src-tauri/src/host.rs` owns the transport-independent workspace, Codex lifecycle, project control, approval state, and bounded event replay.
- `src-tauri/src/discovery.rs` browses and advertises LAN hosts using mDNS/DNS-SD; `lan_store.rs` persists trust and hosting preferences with atomic file replacement.
- `src-tauri/src/lan.rs` provides certificate-verified TLS WebSockets, single-use pairing invitations, persistent device credentials, revocation, and remote request correlation.
- `src-tauri/src/server.rs` owns the child process, initialization, concurrent request correlation, response timeouts, and event forwarding. The child terminates when the app exits.
- `src-tauri/src/workspace.rs` stores project registrations in the supplied app-data directory and restricts editor paths to registered roots, including resolved symbolic links.
- `src/lib/app.svelte.ts` coordinates projects, account state, models, conversations, streaming events, and server requests.
- `src/lib/conversation.ts` reconciles optimistic user messages, incremental output, and completed items.
- `src/lib/components/ui` contains the generated Rhea components; `components.json` retains the registry configuration.

Conversation history and authentication remain owned by Codex. The project registry is `projects.json` in the platform's application-data directory for `com.poti.recodex`; theme, model, and last project preferences use local storage.

The basic editor handles existing UTF-8 text files up to 2 MB. It omits `.git`, `node_modules`, `target`, and `.svelte-kit` from browsing. Reload an open file to see external changes. Each host view keeps independent conversation state. Switching between hosts keeps their views and running work. Unsupported server-initiated tools return an explicit protocol error. LAN hosts are discovered automatically and require the host desktop app to remain open. Full terminal emulation, attachments, and Git operations are outside this implementation.

The bridge follows the [official app-server documentation](https://developers.openai.com/codex/app-server/). Rhea is provided through the [shadcn-svelte registry](https://www.shadcn-svelte.com/docs/changelog).
