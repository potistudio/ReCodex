//! Cross-host IPC bridge. The desktop app calls Tauri IPC; a browser that loads the Web UI
//! served by a host connects to that host's WebSocket bridge instead. Plain browser preview
//! (vite dev without a host) stays read-only.
import { isTauri, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import { type ConfirmDialogOptions, confirm as tauriConfirm } from "@tauri-apps/plugin-dialog";

export type IpcMode = "desktop" | "web" | "preview";

/** Reactive status consumed by the UI chrome (banners, connect buttons). */
const status = $state({
	mode: "desktop" as IpcMode,
	online: true,
});

/** Resolve the mode once by probing the host's own web bridge endpoint. */
let probe: Promise<IpcMode> | undefined;
async function resolveMode(): Promise<IpcMode> {
	if (isTauri()) return "desktop";
	if (probe) return probe;
	probe = (async () => {
		let mode: IpcMode = "preview";
		try {
			const response = await fetch("/api/info", { cache: "no-store" });
			if (response.ok) {
				const info = await response.json();
				if (info?.web === true) mode = "web";
			}
		} catch {
			// No host reachable from this browser: keep the inert preview behaviour.
		}
		status.mode = mode;
		status.online = mode !== "web";
		probe = undefined;
		return mode;
	})();
	return probe;
}

/** Synchronous view of the resolved mode; "desktop" until the web probe completes. */
export function ipcMode(): IpcMode {
	return status.mode;
}
/** Await the environment probe (Tauri, web-served UI, or plain preview). */
export async function resolveIpcMode(): Promise<IpcMode> {
	return resolveMode();
}
/** False only while a running Web UI session holds no socket. */
export function webOnline(): boolean {
	return status.mode !== "web" || status.online;
}

const pending = new Map<
	number,
	{ resolve: (value: unknown) => void; reject: (error: unknown) => void; timer: ReturnType<typeof setTimeout> }
>();
const listeners = new Map<string, Set<(payload: unknown) => void>>();
const REQUEST_TIMEOUT = 65_000;
let socket: WebSocket | undefined;
let connecting: Promise<WebSocket> | undefined;
let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
let retryDelay = 1000;
let nextId = 1;

function deliver(payload: unknown) {
	for (const handler of listeners.get("host-event") ?? []) handler(payload);
}

function rejectPending(error: string) {
	for (const request of pending.values()) {
		clearTimeout(request.timer);
		request.reject(error);
	}
	pending.clear();
}

function scheduleReconnect() {
	if (socket?.readyState === WebSocket.OPEN || reconnectTimer || status.mode !== "web") return;
	reconnectTimer = setTimeout(() => {
		reconnectTimer = undefined;
		retryDelay = Math.min(retryDelay * 2, 30_000);
		void openSocket();
	}, retryDelay);
}

function openSocket(): Promise<WebSocket> {
	if (socket && socket.readyState === WebSocket.OPEN) return Promise.resolve(socket);
	if (connecting) return connecting;
	status.online = false;
	clearTimeout(reconnectTimer);
	reconnectTimer = undefined;
	connecting = new Promise<WebSocket>((resolve, reject) => {
		const address = `${window.location.protocol === "https:" ? "wss:" : "ws:"}//${window.location.host}/api`;
		const next = new WebSocket(address);
		socket = next;
		let settled = false;
		next.onopen = () => {
			settled = true;
			retryDelay = 1000;
			status.online = true;
			resolve(next);
		};
		next.onmessage = (event) => {
			try {
				const message = JSON.parse(String(event.data));
				if (typeof message?.id === "number") {
					const request = pending.get(message.id);
					if (request) {
						pending.delete(message.id);
						clearTimeout(request.timer);
						if ("error" in message) request.reject(String(message.error));
						else request.resolve(message.result);
					}
					return;
				}
				if (message?.method === "host-event") deliver(message.params);
			} catch {
				// Ignore malformed frames; the host controls this endpoint.
			}
		};
		const dropped = () => {
			if (socket === next) socket = undefined;
			status.online = false;
			rejectPending("Web host connection closed. Reconnect to reload host state.");
			if (!settled) {
				settled = true;
				reject("Web host is unreachable");
			}
			scheduleReconnect();
		};
		next.onclose = dropped;
		next.onerror = dropped;
	});
	connecting = connecting.finally(() => {
		connecting = undefined;
	});
	return connecting;
}

/** Invoke a host command over Tauri IPC or the Web bridge, depending on the environment. */
export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	const mode = await resolveMode();
	if (mode === "desktop") return tauriInvoke<T>(cmd, args);
	if (mode === "preview") throw new Error("Browser preview has no IPC. Open the desktop app or the host's Web UI.");
	const channel = await openSocket().catch(() => {
		scheduleReconnect();
		throw new Error("Web UI host is offline. Enable the Web UI in Connections on the host PC.");
	});
	const id = nextId++;
	channel.send(JSON.stringify({ id, command: cmd, params: args ?? {} }));
	return new Promise<T>((resolve, reject) => {
		const timer = setTimeout(() => {
			if (pending.delete(id)) reject("Request timed out. Reconnect and check the chat before retrying a change.");
		}, REQUEST_TIMEOUT);
		pending.set(id, { resolve: (value) => resolve(value as T), reject, timer });
	});
}

/** Event subscription mirroring Tauri's `listen` contract. */
export async function listen<T>(name: string, handler: (event: { payload: T }) => void): Promise<UnlistenFn> {
	if (isTauri()) return tauriListen<T>(name, handler);
	await resolveMode();
	const set = listeners.get(name) ?? new Set();
	set.add(handler as (payload: unknown) => void);
	listeners.set(name, set);
	return () => set.delete(handler as (payload: unknown) => void);
}

/** Confirmation dialog: native in Tauri, `window.confirm` under the Web UI. */
export async function confirm(message: string, options?: ConfirmDialogOptions): Promise<boolean> {
	if (isTauri()) return tauriConfirm(message, options);
	return window.confirm(message);
}
