import { expect, test } from "@playwright/test";

test("hosts explicitly selected projects and isolates remote chats, files, and background turns", async ({ page }) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await page.addInitScript(() => {
		type Args = Record<string, unknown>;
		const native = window as unknown as Args;
		native.isTauri = true;
		native.__TAURI_OS_PLUGIN_INTERNALS__ = { platform: "macos" };
		native.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
		const callbacks = new Map<number, (event: unknown) => void>();
		const listeners = new Set<number>();
		let callbackId = 0;
		let hosting: Args | null = null;
		const files: Record<string, string> = { local: "local file", desktop: "remote file" };
		const project = (id: string) => ({
			path: id === "local" ? "/local" : "remote-project",
			name: id === "local" ? "Local project" : "Remote project",
			historyPaths: [],
		});
		const thread = (id: string) => ({
			id: "same-thread",
			cwd: project(id).path,
			name: id === "local" ? "Local chat" : "Remote chat",
			preview: "",
			turns: [],
			updatedAt: 1,
		});
		const emit = (id: string, event: unknown) => {
			for (const listener of listeners)
				callbacks.get(listener)?.({ event: "codex-event", payload: { hostId: id, event } });
		};
		native.emitHostEvent = emit;
		native.__TAURI_INTERNALS__ = {
			transformCallback(callback: (event: unknown) => void) {
				callbacks.set(++callbackId, callback);
				return callbackId;
			},
			async invoke(command: string, args: Args = {}) {
				const id = (args.hostId as string) ?? "local";
				if (command === "plugin:event|listen") {
					listeners.add(args.handler as number);
					return args.handler;
				}
				if (command === "plugin:event|unlisten") {
					listeners.delete(args.eventId as number);
					return;
				}
				if (command === "host_status") return hosting;
				if (command === "host_start") {
					native.hostedProjects = args.projects;
					hosting = {
						id: "this-pc",
						name: args.name,
						address: args.address,
						devices: [],
						sharedProjects: args.projects,
					};
					return hosting;
				}
				if (command === "host_invite") return "test-invitation";
				if (command === "host_stop") {
					hosting = null;
					return;
				}
				if (command === "host_pair") return { id: "desktop", name: "Desktop" };
				if (command === "server_connect") return;
				if (command === "projects_load") return [project(id)];
				if (command === "files_list") return [{ name: "main.txt", path: "main.txt", isDirectory: false }];
				if (command === "file_read") {
					native.fileHost = id;
					return files[id];
				}
				if (command === "file_save") {
					native.savedFile = args;
					if (args.expected !== files[id]) throw Error("Conflict");
					files[id] = args.content as string;
					return;
				}
				if (command === "server_request") {
					const params = args.params as Args;
					if (args.method === "host/replay")
						return {
							reset: false,
							epoch: `test-${id}`,
							sequence: 0,
							events: [],
							snapshot: { activeTurns: {}, approvals: [] },
						};
					if (args.method === "host/keepalive" || args.method === "host/release") return;
					if (args.method === "model/list")
						return {
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
						};
					if (args.method === "account/read") return { account: null, requiresOpenaiAuth: false };
					if (args.method === "account/rateLimits/read") return null;
					if (args.method === "thread/list") return { data: [thread(id)], nextCursor: null };
					if (args.method === "thread/resume")
						return { thread: thread(id), model: "model", reasoningEffort: "medium" };
					if (args.method === "turn/start") {
						native.lastTurn = { hostId: id, ...params };
						const turn = { id: `turn-${id}`, status: "inProgress" };
						emit(id, { method: "turn/started", params: { threadId: "same-thread", turn } });
						return { turn };
					}
				}
				throw Error(`Unexpected command: ${command} ${args.method ?? ""}`);
			},
		};
	});
	await page.goto("/");
	await page.getByRole("button", { name: "Local chat", exact: true }).click();
	await page.getByRole("textbox", { name: "Message Codex" }).fill("Run locally");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toBeVisible();
	await page.getByRole("button", { name: "Connections", exact: true }).click();
	const start = page.getByRole("button", { name: "Start hosting", exact: true });
	await expect(start).toBeDisabled();
	await page.getByRole("checkbox", { name: "Local project" }).check();
	await start.click();
	await expect(page.getByRole("textbox", { name: "Connection invitation", exact: true })).toHaveValue(
		"test-invitation",
	);
	expect(await page.evaluate(() => (window as unknown as { hostedProjects: string[] }).hostedProjects)).toEqual([
		"/local",
	]);
	await page.getByRole("button", { name: "Stop hosting", exact: true }).click();
	await page.getByRole("textbox", { name: "Other PC's LAN address and port" }).fill("192.168.1.20:43177");
	await page.getByRole("textbox", { name: "Invitation from the other PC" }).fill("test-remote-invitation");
	await page.getByRole("button", { name: "Connect to PC", exact: true }).click();
	await expect(page.getByRole("combobox", { name: "Active host" })).toHaveValue("desktop");
	await page.keyboard.press("Escape");
	await page.getByRole("button", { name: "Remote chat", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toHaveCount(0);
	await page.getByRole("textbox", { name: "Message Codex" }).fill("Run remotely");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await page.evaluate(() => {
		const emit = (window as unknown as { emitHostEvent: (id: string, event: unknown) => void }).emitHostEvent;
		for (const id of ["local", "desktop"])
			emit(id, {
				method: "item/completed",
				params: {
					threadId: "same-thread",
					turnId: `turn-${id}`,
					item: {
						id: "same-answer",
						type: "agentMessage",
						text: id === "local" ? "Local background reply" : "Remote reply",
					},
				},
			});
		emit("desktop", {
			method: "turn/completed",
			params: { threadId: "same-thread", turn: { id: "turn-desktop", status: "completed" } },
		});
	});
	await expect(page.getByText("Remote reply", { exact: true })).toBeVisible();
	await expect(page.getByText("Local background reply", { exact: true })).toHaveCount(0);
	await page.getByRole("button", { name: "Files", exact: true }).click();
	await page.getByRole("button", { name: "main.txt", exact: true }).click();
	const editor = page.getByRole("textbox", { name: "Edit main.txt" });
	await expect(editor).toHaveValue("remote file");
	await editor.fill("edited remotely");
	await page.getByRole("button", { name: "Save", exact: true }).click();
	expect(await page.evaluate(() => (window as unknown as { savedFile: unknown }).savedFile)).toMatchObject({
		hostId: "desktop",
		root: "remote-project",
		content: "edited remotely",
		expected: "remote file",
	});
	await page.getByRole("combobox", { name: "Active host" }).selectOption("local");
	await expect(page.getByText("Local background reply", { exact: true })).toBeVisible();
	await expect(page.getByRole("button", { name: "Stop generation" })).toBeVisible();
	await expect(page.getByText("Remote reply", { exact: true })).toHaveCount(0);
	expect(errors).toEqual([]);
});
