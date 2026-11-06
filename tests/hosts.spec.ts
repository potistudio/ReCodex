import { expect, test } from "@playwright/test";
import { installTauriMock, type TauriMock } from "./support/tauri-mock";

test("hosts explicitly selected projects and isolates remote chats, files, and background turns", async ({ page }) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await installTauriMock(page);
	await page.addInitScript(() => {
		type Args = Record<string, unknown>;
		const native = window as unknown as Args;
		const mock = native.tauriMock as TauriMock;
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
		native.emitHostEvent = (id: string, event: unknown) => mock.emitHost(event, id);
		mock.commands.host_status = () => hosting;
		mock.commands.host_start = (args) => {
			native.hostedProjects = args.projects;
			hosting = {
				id: "this-pc",
				name: args.name,
				address: args.address,
				devices: [],
				sharedProjects: args.projects,
			};
			return hosting;
		};
		mock.commands.host_invite = () => "test-invitation";
		mock.commands.host_stop = () => {
			hosting = null;
		};
		mock.commands.host_pair = (args) => {
			native.pairAddress = args.address;
			return { id: "desktop", name: "Desktop" };
		};
		mock.commands.projects_load = (_args, id) => [project(id)];
		mock.commands.files_list = () => [{ name: "main.txt", path: "main.txt", isDirectory: false }];
		mock.commands.file_read = (_args, id) => {
			native.fileHost = id;
			return files[id];
		};
		mock.commands.file_save = (args, id) => {
			native.savedFile = args;
			if (args.expected !== files[id]) throw Error("Conflict");
			files[id] = args.content as string;
		};
		mock.methods["thread/list"] = (_params, id) => ({ data: [thread(id)], nextCursor: null });
		mock.methods["thread/resume"] = (_params, id) => ({
			thread: thread(id),
			model: "model",
			reasoningEffort: "medium",
		});
		mock.methods["turn/start"] = (params, id) => {
			native.lastTurn = { hostId: id, ...params };
			const turn = { id: `turn-${id}`, status: "inProgress" };
			mock.emitHost({ method: "turn/started", params: { threadId: "same-thread", turn } }, id);
			return { turn };
		};
	});
	await page.goto("/");
	await page.getByRole("button", { name: "Local chat", exact: true }).click();
	await page.getByRole("textbox", { name: "Message ReCodex" }).fill("Run locally");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toBeVisible();
	await page.getByRole("button", { name: "Connections", exact: true }).click();
	await expect(page.getByRole("button", { name: /^(Take control|Release control)$/ })).toHaveCount(0);
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

	await page.getByRole("textbox", { name: "Invitation from the other PC" }).fill("test-remote-invitation");
	await page.getByRole("button", { name: "Connect to PC", exact: true }).click();
	await expect(page.getByRole("combobox", { name: "Active host" })).toHaveValue("desktop");
	expect(await page.evaluate(() => (window as unknown as { pairAddress: unknown }).pairAddress)).toBeNull();
	await page.keyboard.press("Escape");
	await page.getByRole("button", { name: "Remote chat", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toHaveCount(0);
	await page.getByRole("textbox", { name: "Message ReCodex" }).fill("Run remotely");
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
