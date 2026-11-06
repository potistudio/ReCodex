import type { Page } from "@playwright/test";

type Args = Record<string, unknown>;

/**
 * The fake Tauri IPC layer that `installTauriMock` puts on `window.tauriMock`.
 *
 * It replaces `window.__TAURI_INTERNALS__`, so the web client runs as if inside the desktop shell. A spec adds its own
 * scenario in a second `page.addInitScript` and registers handlers here; anything it does not handle falls back to
 * the defaults below, and unknown commands throw so a missing mock fails loudly.
 */
export interface TauriMock {
	/** Handlers for IPC commands other than `server_request`, keyed by command name. They take `args` and the host id. */
	commands: Record<string, (args: Args, hostId: string) => unknown>;
	/** Handlers for `server_request` methods, keyed by method name. They take the request `params` and the host id. */
	methods: Record<string, (params: Args, hostId: string) => unknown>;
	/** Delivers `payload` to every listener registered for the Tauri event `event`. */
	emit(event: string, payload: unknown): void;
	/** Delivers a host event, `{ method, params }`, as the shell would for the given host. */
	emitHost(event: unknown, hostId?: string): void;
}

/**
 * Installs the fake Tauri IPC layer before the page loads. Call it before `page.goto`.
 *
 * Defaults, all overridable through `mock.commands` and `mock.methods`:
 * - `hosts_list`, `host_status`, `server_connect`, and event listen/unlisten: nothing registered, nothing hosted.
 * - `host/replay`: an empty replay whose epoch is `test-<hostId>`.
 * - `provider/list`, `account/read`: one available Command Code provider, signed out.
 * - `model/list`: one model named "Model". `thread/list`: no threads.
 */
export async function installTauriMock(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const native = window as unknown as Record<string, unknown>;
		native.isTauri = true;
		native.__TAURI_OS_PLUGIN_INTERNALS__ = { platform: "macos" };
		native.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

		const callbacks = new Map<number, (event: unknown) => void>();
		const listeners = new Map<number, string>();
		let callbackId = 0;

		const mock: TauriMock = {
			commands: {},
			methods: {},
			emit(event, payload) {
				for (const [handler, name] of listeners)
					if (name === event) callbacks.get(handler)?.({ event, payload });
			},
			emitHost(event, hostId = "local") {
				mock.emit("host-event", { hostId, event });
			},
		};
		native.tauriMock = mock;

		const defaultCommands: TauriMock["commands"] = {
			hosts_list: () => ({ registered: [], discovered: [], discoveryError: null, hostingError: null }),
			host_status: () => null,
			server_connect: () => undefined,
			"plugin:event|listen": (args) => {
				listeners.set(args.handler as number, args.event as string);
				return args.handler;
			},
			"plugin:event|unlisten": (args) => {
				listeners.delete(args.eventId as number);
			},
		};
		const defaultMethods: TauriMock["methods"] = {
			"host/replay": (_params, hostId) => ({
				reset: false,
				epoch: `test-${hostId}`,
				sequence: 0,
				events: [],
				snapshot: { activeTurns: {}, approvals: [] },
			}),
			"provider/list": () => [{ id: "recodex", label: "Command Code", available: true }],
			"account/read": () => ({
				available: true,
				connection: "Command Code",
				loginPending: false,
				signedIn: false,
			}),
			"model/list": () => ({
				data: [
					{
						id: "model",
						model: "model",
						displayName: "Model",
						isDefault: true,
						defaultReasoningEffort: "medium",
						supportedReasoningEfforts: [{ reasoningEffort: "medium", description: "Medium" }],
					},
				],
				nextCursor: null,
			}),
			"thread/list": () => ({ data: [], nextCursor: null }),
		};

		native.__TAURI_INTERNALS__ = {
			transformCallback(callback: (event: unknown) => void) {
				callbacks.set(++callbackId, callback);
				return callbackId;
			},
			async invoke(command: string, args: Args = {}) {
				const hostId = (args.hostId as string | undefined) ?? "local";
				if (command === "server_request") {
					const method = args.method as string;
					const handler = mock.methods[method] ?? defaultMethods[method];
					if (!handler) throw new Error(`Unexpected method: ${method}`);
					return handler((args.params as Args | undefined) ?? {}, hostId);
				}
				const handler = mock.commands[command] ?? defaultCommands[command];
				if (!handler) throw new Error(`Unexpected command: ${command}`);
				return handler(args, hostId);
			},
		};
	});
}
