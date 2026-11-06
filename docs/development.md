# Development

## Repository layout

| Path                               | Contents                                                                                                                                |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `web/`                             | The Svelte web client and the Vite root. The build goes to `web/dist` (git-ignored).                                                    |
| `desktop/`                         | The Tauri shell and the Rust host.                                                                                                      |
| `tests/`                           | Playwright E2E specs. `tests/support/tauri-mock.ts` is the Tauri IPC mock they share.                                                   |
| `docs/`                            | Design and behavior documents. See [Documentation](../README.md#documentation).                                                         |
| `scripts/`                         | `pull-refs.mjs` (`pnpm pull:refs`) and `check-versions.mjs` (part of `pnpm check`).                                                     |
| `refs/`                            | Git-ignored local clones that the analysis documents cite. `pnpm pull:refs` fast-forwards the repositories already there.               |
| `mise.toml`, `rust-toolchain.toml` | Pinned tool versions. See [Tool versions](#tool-versions).                                                                              |
| `biome.jsonc`, `rustfmt.toml`      | Formatter and linter settings for the frontend and for Rust.                                                                            |
| `vite.config.js`                   | Vite and Vitest settings. `tsconfig.json`, `svelte.config.js`, `playwright.config.ts`, and `components.json` configure the other tools. |

Unit tests sit next to the code as `web/src/**/*.test.ts` (Vitest) and inside the Rust modules. Playwright specs are `*.spec.ts`, and Vitest ignores them.

## Tool versions

- Node.js, pnpm, and Biome are pinned in `mise.toml`. `package.json` repeats Node.js and pnpm in `engines.node` and `packageManager`, and the `$schema` URL in `biome.jsonc` repeats the Biome version, so update them together.
- Rust is pinned in `rust-toolchain.toml`, which rustup reads. It is not listed in `mise.toml`, because mise's Rust plugin failed to install on Windows and then broke every `mise run`.
- The app version lives in `package.json`. `desktop/tauri.conf.json` reads it from there, and the settings dialog gets it through a Vite `define`. `desktop/Cargo.toml` cannot read `package.json`, so `scripts/check-versions.mjs` makes `pnpm check` fail when the two differ.

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
