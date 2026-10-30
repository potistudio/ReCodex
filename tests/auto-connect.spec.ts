import { expect, test } from "@playwright/test";

test("restores trusted hosts, reconnects on discovery without resending turns, and stops after forgetting", async ({
	page,
}) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await page.addInitScript(() => {
		type Args = Record<string, unknown>;
		const native = window as unknown as Args;
		native.isTauri = true;
		native.__TAURI_OS_PLUGIN_INTERNALS__ = { platform: "macos" };
		native.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
		localStorage.setItem("recodex-host", "desktop");
		let online = false;
		let registered = true;
		let running = false;
		let address = "192.168.1.20:43177";
		const counts = { connect: 0, start: 0 };
		native.autoCounts = counts;
		const callbacks = new Map<number, (event: unknown) => void>();
		const listeners = new Map<number, string>();
		let callback = 0;
		const emit = (event: string, payload: unknown) => {
			for (const [handler, name] of listeners) if (name === event) callbacks.get(handler)?.({ event, payload });
		};
		native.announcePc = () => {
			online = true;
			address = "192.168.1.21:43177";
			emit("hosts-changed", {});
		};
		native.disconnectPc = () => {
			online = false;
			emit("codex-event", { hostId: "desktop", event: { method: "recodex/disconnected", params: {} } });
		};
		const thread = {
			id: "chat",
			name: "Desktop chat",
			preview: "Desktop chat",
			cwd: "project-id",
			updatedAt: 1,
			turns: [],
		};
		native.__TAURI_INTERNALS__ = {
			transformCallback(fn: (event: unknown) => void) {
				callbacks.set(++callback, fn);
				return callback;
			},
			async invoke(command: string, args: Args = {}) {
				const id = args.hostId ?? "local";
				if (command === "plugin:event|listen") {
					listeners.set(args.handler as number, args.event as string);
					return args.handler;
				}
				if (command === "plugin:event|unlisten") {
					listeners.delete(args.eventId as number);
					return;
				}
				if (command === "hosts_list")
					return {
						registered: registered ? [{ id: "desktop", name: "Desktop" }] : [],
						discovered: online ? [{ id: "desktop", name: "Desktop", addresses: [address] }] : [],
						discoveryError: null,
						hostingError: null,
					};
				if (command === "host_status") return null;
				if (command === "host_forget") {
					registered = false;
					return;
				}
				if (command === "server_connect") {
					if (id === "desktop") {
						counts.connect++;
						if (!online) throw Error("PC unavailable");
					}
					return;
				}
				if (command === "projects_load")
					return id === "desktop" ? [{ name: "Desktop project", path: "project-id", historyPaths: [] }] : [];
				if (command !== "server_request") throw Error(`Unexpected command ${command}`);
				const params = args.params as Args;
				if (args.method === "model/list")
					return {
						data: [
							{
								id: "m",
								model: "m",
								displayName: "Model",
								isDefault: true,
								defaultReasoningEffort: "medium",
								supportedReasoningEfforts: [{ reasoningEffort: "medium", description: "Medium" }],
							},
						],
						nextCursor: null,
					};
				if (args.method === "account/read") return { account: null, requiresOpenaiAuth: false };
				if (args.method === "account/rateLimits/read") return null;
				if (args.method === "thread/list") return { data: [thread], nextCursor: null };
				if (args.method === "thread/resume") return { thread, model: "m", reasoningEffort: "medium" };
				if (args.method === "host/replay")
					return {
						reset: false,
						epoch: `test-${id}`,
						sequence: 0,
						events: [],
						snapshot: {
							activeTurns: running ? { chat: { id: "turn", status: "inProgress" } } : {},
							approvals: [],
						},
					};
				if (["host/keepalive", "host/release"].includes(args.method as string)) return;
				if (args.method === "turn/start") {
					if (params.threadId !== "chat") throw Error("Wrong chat");
					counts.start++;
					running = true;
					const turn = { id: "turn", status: "inProgress" };
					emit("codex-event", {
						hostId: "desktop",
						event: { method: "turn/started", params: { threadId: "chat", turn } },
					});
					return { turn };
				}
				throw Error(`Unexpected method ${args.method}`);
			},
		};
	});
	await page.goto("/");
	const selected = page.getByRole("combobox", { name: "Active host" });
	await expect(selected).toHaveValue("desktop");
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { autoCounts: { connect: number } }).autoCounts.connect))
		.toBeGreaterThan(0);
	await page.evaluate(() => (window as unknown as { announcePc: () => void }).announcePc());
	await page.getByRole("button", { name: "Desktop chat", exact: true }).click();
	await page.getByRole("textbox", { name: "Message Codex" }).fill("Run once");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toBeVisible();
	await page.evaluate(() => (window as unknown as { disconnectPc: () => void }).disconnectPc());
	await expect(page.getByRole("button", { name: "Connect to Codex", exact: true })).toBeVisible();
	await page.evaluate(() => (window as unknown as { announcePc: () => void }).announcePc());
	await expect(page.getByRole("button", { name: "Connect to Codex", exact: true })).toHaveCount(0);
	expect(await page.evaluate(() => (window as unknown as { autoCounts: { start: number } }).autoCounts.start)).toBe(
		1,
	);
	await page.getByRole("button", { name: "Connections", exact: true }).click();
	await expect(page.getByRole("region", { name: "Nearby PCs" })).toContainText("Connected automatically");
	await page.keyboard.press("Escape");
	// The backend's saved registration is present after the UI/app is recreated.
	await page.reload();
	await expect(selected).toHaveValue("desktop");
	await page.evaluate(() => (window as unknown as { announcePc: () => void }).announcePc());
	await page.getByRole("button", { name: "Desktop chat", exact: true }).click();
	await page.getByRole("button", { name: "Connections", exact: true }).click();
	await page.getByRole("button", { name: "Disconnect and forget Desktop", exact: true }).click();
	await expect(selected).toHaveValue("local");
	const count = await page.evaluate(
		() => (window as unknown as { autoCounts: { connect: number } }).autoCounts.connect,
	);
	await page.waitForTimeout(2200);
	expect(
		await page.evaluate(() => (window as unknown as { autoCounts: { connect: number } }).autoCounts.connect),
	).toBe(count);
	expect(errors).toEqual([]);
});
