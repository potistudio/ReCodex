# LAN and Tailscale hosts

Each ReCodex desktop app is a client and can optionally host explicitly selected local projects. PCs connect directly; no coordinator, file share, or relay is required. Each PC can host and connect at the same time.

## Set up hosting

Run the desktop app on each PC and sign in to Command Code (or configure another model API) on the PCs that will host projects. Open the projects you want to use on their respective PCs.

1. On the PC that owns a project, open **Connections**, select the projects to share, and choose **Start hosting**. The default listen address is `0.0.0.0:43177`.
2. Other PCs find it automatically under **Nearby PCs** using mDNS. Copy its **Connection invitation** to a PC you want to authorize through a trusted channel. Invitations expire after ten minutes and can be used once.
3. Paste the invitation into **Connect to another PC** and choose **Connect to PC**. ReCodex locates the matching host automatically; no IP address is required.
4. Registered PCs reconnect automatically on launch, after connection loss, and when discovery finds a changed address. **Active host** restores your last selection and switches between connected PCs. Repeat the initial invitation exchange in the opposite direction for mutual hosting.

Automatic discovery uses UDP 5353 on the local link; allow local-network access and the listening TCP port on your private LAN when requested by the OS. If multicast is unavailable, **Manual address (optional)** accepts a LAN IP or hostname and port; registered connections also retry their saved addresses. `0.0.0.0` is a bind address, not a destination. Hosting runs while the host app is open and resumes automatically on the next launch until you choose **Stop hosting**.

## Ownership and routing

`Host` in `desktop/src/host.rs` owns the local agent harness, workspace registry, active turns, approvals, ordered operations, and event history. It takes a storage directory rather than a Tauri handle. `workspace.rs` contains filesystem and registry operations. Tauri commands in `lib.rs` adapt these operations for the desktop UI.

Every UI `App` instance has a host ID. Each instance keeps its own projects, model catalog, provider status, chat state, event cursor, and saved project/model preferences. File IPC includes the same host ID. Events carry their source host ID, so equal thread or item IDs on different hosts cannot mix. Changing the selected host does not cancel the previous host's turn.

Local calls go directly to `Host`. Remote calls travel from the local Rust backend over TLS WebSockets to the selected host's `Host`. The webview continues to use local Tauri IPC; its CSP does not grant general LAN access. The agent harness runs inside the host process.

The host's model API credentials, filesystem, and environment are used for execution. Credentials and source trees are not synchronized between PCs. Users configure the model API and register/relocate projects on the PC that owns them.

## Opt-in hosting and pairing

Hosting starts after the user chooses registered projects and enables it in Connections. The default TCP bind address is `0.0.0.0:43177`; a specific interface or IPv6 bind address can also be entered. This preference is saved and restored on subsequent app launches. Stop hosting disables that saved preference as well as the current listener. The host app still needs to remain open.

Every desktop app browses `_recodex._tcp.local.` using mDNS/DNS-SD. An enabled host advertises its stable ID, display name, protocol version, addresses, and listening port. Publishing follows the listener's address family and interface; address checks refresh network interfaces every five seconds. A stopped host withdraws its advertisement. Nearby PCs excludes this PC and unsupported protocol versions, updates resolved addresses, and removes expired/withdrawn services. No credential, certificate, project path, or conversation is advertised.

Pasting an invitation automatically selects discovered addresses for the matching host ID. A manual address remains available when multicast is blocked. Explicit manual addresses take priority over discovery. Registered connections retain their first saved address (including a MagicDNS name) ahead of current discovered addresses, with other saved addresses as fallbacks. Discovery changes reachability only: they never replace a saved certificate pin, authorize an unknown PC, or overwrite an existing credential. The TLS check occurs before sending authentication data to a newly discovered address.

A local certificate/private key and stable host ID are generated on first use and saved as `lan-identity.json` in the application's data directory. On Unix, this file is created with mode `0600`. The private key is never included in invitations.

An invitation contains the public certificate, host ID, display name, protocol version, and a random bearer secret. Transfer it through a trusted channel. The host retains a SHA-256 verifier, expires invitations after ten minutes, and consumes an invitation after one successful pairing. Creating a replacement invalidates an unused invitation.

The client trusts only the certificate supplied in that invitation and performs normal TLS certificate and name validation against the certificate's `recodex.local` identity. The TCP destination can be the host's LAN IP; the TLS identity is independent of that address. No certificate-verification bypass is used.

Pairing exchanges the invitation for a separate random device credential. Credentials are exchanged inside TLS, compared using constant-time digest comparisons, and persisted in the native app's `lan-settings.json` rather than URLs, logs, or browser storage. It is written by atomic replacement with owner-only permissions (`0600`) on Unix; Windows uses the per-user application-data directory's access controls. Client credentials are stored as bearer secrets in that file, not encrypted with an OS keychain; host-side device records contain only SHA-256 verifiers. Each invitation grants operation and approval privileges across the projects shared by that host, including after a restart. Connections lists devices and can revoke them individually; revocation closes their connections and cancels requests still waiting for the host's operation gate.

Device records, remote connection profiles, and host preferences survive app restarts. Stopping hosting keeps paired devices registered for the next time hosting is enabled. Revocation removes the persisted verifier before disconnecting that device, so a revoked credential cannot become valid after a restart. Forgetting a client connection removes its saved credential and stops retries; a late reconnect cannot write the deleted profile back. A fresh invitation can replace a saved registration when the user needs to authorize that PC again. The certificate/host identity remains stable.

## Connecting over Tailscale

ReCodex uses the OS's Tailscale connection; it does not install Tailscale, manage its login, or require its CLI. Connect both PCs to a tailnet that permits access between them. Multicast/mDNS discovery does not span Tailscale, so use an explicit destination for the first pairing:

1. On the host, select projects in **Connections** and start hosting. The default `0.0.0.0:43177` accepts IPv4 connections on both LAN and Tailscale interfaces. To listen only on Tailscale, use the host's Tailscale IPv4 address, such as `100.101.102.103:43177`. For IPv6-only hosting, use an IPv6 bind address such as `[fd7a:115c:a1e0::1]:43177`.
2. Allow the listener's TCP port in the host firewall and [tailnet access rules](https://tailscale.com/kb/1018/acls). Do not expose it using router port forwarding or Tailscale Funnel.
3. Transfer a fresh invitation through a trusted channel. On the client, enter the host's Tailscale IP, or its [MagicDNS](https://tailscale.com/kb/1081/magicdns) short name / fully qualified name (for example `desktop.example.ts.net`), and paste the invitation. MagicDNS must be enabled and available to the client's OS resolver when using names.
4. Connect once. ReCodex saves the address and device credential and retries automatically after restarts or interruptions. A saved hostname is resolved again on each attempt. If an IP changes, pair again with a new invitation and address; a stable MagicDNS name avoids that requirement.

The port defaults to `43177` when omitted. Custom ports use `hostname:port` or `[IPv6]:port`; a bare IPv6 address uses the default port. Enter an address, not an `https://` or `wss://` URL. Each destination has a ten-second TCP/TLS/WebSocket connection timeout to accommodate relay setup. No application-level Tailscale peer discovery is performed, and Tailscale connectivity alone never authorizes a peer: the same pinned TLS certificate, invitation, project boundaries, and device revocation apply.

## Access boundaries

The LAN protocol has an explicit method allowlist. Remote clients cannot change credentials or configuration, register arbitrary directories, or call methods outside the allowlist. The host's approval policy applies to every chat; clients cannot supply policy overrides. Chat input is restricted to text.

Remote project references are opaque IDs derived from registered roots. Each filesystem request resolves the ID against the current shared-project list. Relative file paths are canonicalized and checked against the registered root, including resolved symlinks. Existing UTF-8/2 MB editor limits remain in effect.

Thread requests first read and validate the thread's working directory on the host. Thread list queries are scoped to the selected root. Streaming events, pending approvals, active-turn snapshots, and replay are filtered to shared threads. Diagnostic stderr is not sent to remote peers.

Pairing authorizes use of the host's model API and approval UI. As with local operation, an authorized operator can approve file changes and commands requested by the agent, and approved commands run without a sandbox. Sharing a project is an access boundary for the ReCodex API, not an OS-level isolation boundary for all capabilities of that user account or approved tools.

Browser `Origin` requests are rejected during the WebSocket handshake. TLS and pairing handshakes have timeouts. The listener bounds concurrent connections, incoming message size, per-peer request queues, and outgoing queues. Slow or overflowing event consumers disconnect and reload state on reconnect.

## Concurrent operations

All paired devices and the local UI can send messages, stop turns, answer approvals, and save files without acquiring a device-level lease. The host orders operation acceptance through a gate. A second turn in the same running chat is rejected while its draft remains available in the sending UI; a different chat in the same project can run independently. Reading a running chat does not reconfigure it. Deleting a running chat is rejected until its turn stops.

Any authorized device can answer a pending approval. The host accepts the answer once and broadcasts its resolution so other views remove the prompt. A device that races with an already accepted answer receives an already-resolved error. Stopping a turn from another device broadcasts the completed/interrupted turn state to every subscribed view.

File saves compare the current disk contents with the version the editor read. ReCodex saves are serialized, so a stale second save fails rather than overwriting another ReCodex client's edit. This detects external edits present at comparison time; it is not a transaction with arbitrary external editors. Successful saves publish a project-scoped change event to all eligible clients. Their file browsers refresh without overwriting open editor buffers; a stale editor still needs to reload before saving.

The host retains pending approval requests and accepts an answer once, then broadcasts their resolution. Reconnecting does not auto-approve or resubmit them.

## Disconnects and recovery

Registered remote host views are recreated automatically on launch, and the UI restores the last selected host. After connection loss, each view retries connection and state synchronization with a one-second initial delay, doubling to a maximum thirty-second retry delay. Discovering a known PC or an updated address wakes its retry immediately. Forgetting a host disposes its retry loop. Reconnection restores transport and conversation state; it does not resubmit chat messages or approvals.

Disconnecting a client, switching hosts, or stopping LAN hosting does not stop the host's turn. A pending approval waits for an authorized device to answer. Exiting the host app stops running turns and ends LAN hosting; the next launch records them as interrupted.

Host events have a process epoch and sequence number. The host retains up to 8,192 events within a 4 MiB history budget. Reconnection replays missing accessible events and refreshes pending approvals and active turns. If the process changed or the cursor fell outside the retained history, the UI reloads conversation state from the host's session ledger. Live events received during this handshake are queued and reconciled against the replay cursor.

Mutations are not automatically retried after a timeout or lost response: they may already have been accepted. The UI asks users to reconnect and inspect the chat before resubmitting. Cancellation removes request-correlation entries; a timed-out or disconnected caller does not imply that an already accepted operation was cancelled.

## Verification

The macOS bundle declares its Bonjour service and local-network usage description in `desktop/Info.plist`. OS local-network permissions and firewall rules still apply. Discovery failure is shown in Connections; a manual address remains available.

Rust tests use a scripted model that requests one command approval per turn, with no credentials, external network access, or model calls. They cover project/method boundaries, cross-device local/remote editing, concurrent-save conflicts, same-chat start races, independent chats, cross-device stopping, single-answer approval races and notifications, replay privacy, certificate validation, invitation expiry/reuse, revocation, real TLS WebSocket chat/file/reconnect flows, pairing across host/client restarts, durable revocation/forgetting, and discovery record changes/removal. The opt-in `live_mdns_discovery_and_removal` test runs real publisher/browser daemons on a separate UDP multicast port.

The manual-address unit test covers Tailscale IPv4/IPv6, MagicDNS names, default/custom ports, and malformed destinations. The TLS transport test also connects through native hostname resolution. Physical tailnet connectivity and firewall/access-rule behavior require a two-PC smoke test.

The Playwright host test runs LAN-discovery and explicit MagicDNS-address variants and mocks Tauri IPC to verify explicit project selection, host switching during an active local turn, isolation of equal thread/item IDs across hosts, routing of remote editor saves, restoring registered PCs without an invitation, automatic reconnection after discovery updates, avoiding turn resubmission, and stopping retries after forgetting. It complements the Rust transport tests; it does not emulate OS firewall behavior or prove a connection across two physical PCs.
