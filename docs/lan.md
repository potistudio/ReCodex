# LAN hosts

Each ReCodex desktop app is a client and can optionally host explicitly selected local projects. PCs connect directly; no coordinator, file share, or relay is required. Each PC can host and connect at the same time.

## Ownership and routing

`Host` in `src-tauri/src/host.rs` owns the local Codex process, workspace registry, active turns, approvals, project control, and event history. It takes a storage directory rather than a Tauri handle. `workspace.rs` contains filesystem and registry operations. Tauri commands in `lib.rs` adapt these operations for the desktop UI.

Every UI `App` instance has a host ID. Each instance keeps its own projects, model catalog, account, chat state, event cursor, and saved project/model preferences. File IPC includes the same host ID. Events carry their source host ID, so equal thread or item IDs on different hosts cannot mix. Changing the selected host does not cancel the previous host's turn.

Local calls go directly to `Host`. Remote calls travel from the local Rust backend over TLS WebSockets to the selected host's `Host`. The webview continues to use local Tauri IPC; its CSP does not grant general LAN access. Codex is kept behind the host and uses its existing stdio protocol.

The host's Codex account, CLI, filesystem, sandbox, and environment are used for execution. Authentication files and source trees are not synchronized between PCs. Users sign in and register/relocate projects on the PC that owns them.

## Opt-in hosting and pairing

Hosting starts only after the user chooses registered projects and enables it in Connections. The default TCP bind address is `0.0.0.0:43177`; a specific interface or IPv6 bind address can also be entered. Peers enter an actual host IP/name and port separately.

A local certificate/private key and stable host ID are generated on first use and saved as `lan-identity.json` in the application's data directory. On Unix, this file is created with mode `0600`. The private key is never included in invitations.

An invitation contains the public certificate, host ID, display name, protocol version, and a random bearer secret. Transfer it through a trusted channel. The host retains a SHA-256 verifier, expires invitations after ten minutes, and consumes an invitation after one successful pairing. Creating a replacement invalidates an unused invitation.

The client trusts only the certificate supplied in that invitation and performs normal TLS certificate and name validation against the certificate's `recodex.local` identity. The TCP destination can be the host's LAN IP; the TLS identity is independent of that address. No certificate-verification bypass is used.

Pairing exchanges the invitation for a separate random device credential. Credentials are exchanged inside TLS, compared using constant-time digest comparisons, and held in Rust memory rather than URLs, logs, or browser storage. Each invitation currently grants operation and approval privileges across all projects selected for that hosting session. Connections lists devices and can revoke them individually; revocation closes their connections and cancels requests still waiting for the host's operation gate.

Device records and remote connection registrations are session-only. Stopping hosting invalidates its devices. Restarting either application requires a new invitation; forget an existing remote entry before pairing it again. The certificate/host identity survives app restarts.

## Access boundaries

The LAN protocol has an explicit method allowlist. Remote clients cannot change account credentials, write global Codex configuration, register arbitrary directories, or forward arbitrary Codex methods. New and resumed threads use `workspace-write` and `on-request`; client-supplied policy/configuration overrides are discarded. Chat input is restricted to text.

Remote project references are opaque IDs derived from registered roots. Each filesystem request resolves the ID against the current shared-project list. Relative file paths are canonicalized and checked against the registered root, including resolved symlinks. Existing UTF-8/2 MB editor limits remain in effect.

Thread requests first read and validate the thread's working directory on the host. Thread list queries are scoped to the selected root. Streaming events, pending approvals, active-turn snapshots, and replay are filtered to shared threads. Diagnostic stderr is not sent to remote peers.

Pairing authorizes use of the host's Codex account and approval UI. As with local operation, an authorized operator can approve elevated actions requested by Codex. Sharing a project is an access boundary for the ReCodex API, not an OS-level isolation boundary for all capabilities of that user account or approved tools.

Browser `Origin` requests are rejected during the WebSocket handshake. TLS and pairing handshakes have timeouts. The listener bounds concurrent connections, incoming message size, per-peer request queues, and outgoing queues. Slow or overflowing event consumers disconnect and reload state on reconnect.

## Project control

A project has at most one current controlling session. The first mutation acquires its control lease. Local editor writes, remote file saves, thread creation/deletion, turn submission/interruption, and approval responses all check the same host-side lease. Observing a conversation does not acquire control.

An authorized device can explicitly use Take control to take over the selected project. The previous holder's subsequent changes are rejected by the same lease check. The holder can use Release control to hand over all of its project leases. Connected clients send heartbeats every ten seconds. Leases expire after 35 seconds without renewal; remote socket closure also releases that session's leases. An old session cannot renew or release a newer session's lease. All operations pass through a host gate so ownership checks and accepted mutations are ordered.

File saves compare the current disk contents with the version the editor read. ReCodex saves are serialized, so a stale second save fails rather than overwriting another ReCodex client's edit. This detects external edits present at comparison time; it is not a transaction with arbitrary external editors.

Approval IDs include the Codex process generation. The host retains pending requests and accepts an answer once, then broadcasts their resolution. Reconnecting does not auto-approve or resubmit them. Unsupported server-initiated tools receive an explicit protocol error from the host even without an attached UI.

## Disconnects and recovery

Disconnecting a client, switching hosts, or stopping LAN hosting does not stop the host's Codex turn. A pending approval waits for a permitted controlling device. Exiting the host app terminates the owned Codex child process and ends LAN hosting.

Host events have a process epoch and sequence number. The host retains up to 8,192 events within a 4 MiB history budget. Reconnection replays missing accessible events and refreshes pending approvals and active turns. If the process changed or the cursor fell outside the retained history, the UI reloads conversation state from Codex. Live events received during this handshake are queued and reconciled against the replay cursor.

Mutations are not automatically retried after a timeout or lost response: they may already have been accepted. The UI asks users to reconnect and inspect the chat before resubmitting. Cancellation removes request-correlation entries; a timed-out or disconnected caller does not imply that an already accepted Codex operation was cancelled.

## Verification

Rust tests start a deterministic Node.js app-server fixture with no sign-in, external network access, or model calls. They cover project/method boundaries, sandbox policy enforcement, ownership including local editing, approval resolution, replay privacy, certificate validation, invitation expiry/reuse, revocation, and real TLS WebSocket chat/file/reconnect flows.

The Playwright host test mocks Tauri IPC to verify explicit project selection, host switching during an active local turn, isolation of equal thread/item IDs across hosts, and routing of remote editor saves. It complements the Rust transport tests; it does not emulate OS firewall behavior or prove a connection across two physical PCs.
