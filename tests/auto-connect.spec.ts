import { expect, test } from "@playwright/test";
import { installTauriMock, type TauriMock } from "./support/tauri-mock";

test("restores trusted hosts, reconnects on discovery without resending turns, and stops after forgetting", async ({
	page,
}) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await installTauriMock(page);
	await page.addInitScript(() => {
		const native = window as unknown as Record<string, unknown>;
		const mock = native.tauriMock as TauriMock;
		localStorage.setItem("recodex-host", "desktop");
		let online = false;
		let registered = true;
		let running = false;
		let address = "192.168.1.20:43177";
		const counts = { connect: 0, start: 0 };
		native.autoCounts = counts;
		native.announcePc = () => {
			online = true;
			address = "192.168.1.21:43177";
			mock.emit("hosts-changed", {});
		};
		native.disconnectPc = () => {
			online = false;
			mock.emitHost({ method: "recodex/disconnected", params: {} }, "desktop");
		};
		const thread = {
			id: "chat",
			name: "Desktop chat",
			preview: "Desktop chat",
			cwd: "project-id",
			updatedAt: 1,
			turns: [],
		};
		mock.commands.hosts_list = () => ({
			registered: registered ? [{ id: "desktop", name: "Desktop" }] : [],
			discovered: online ? [{ id: "desktop", name: "Desktop", addresses: [address] }] : [],
			discoveryError: null,
			hostingError: null,
		});
		mock.commands.host_forget = () => {
			registered = false;
		};
		mock.commands.server_connect = (_args, id) => {
			if (id === "desktop") {
				counts.connect++;
				if (!online) throw Error("PC unavailable");
			}
		};
		mock.commands.projects_load = (_args, id) =>
			id === "desktop" ? [{ name: "Desktop project", path: "project-id", historyPaths: [] }] : [];
		mock.methods["model/list"] = () => ({
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
		});
		mock.methods["thread/list"] = () => ({ data: [thread], nextCursor: null });
		mock.methods["thread/resume"] = () => ({ thread, model: "m", reasoningEffort: "medium" });
		mock.methods["host/replay"] = (_params, id) => ({
			reset: false,
			epoch: `test-${id}`,
			sequence: 0,
			events: [],
			snapshot: {
				activeTurns: running ? { chat: { id: "turn", status: "inProgress" } } : {},
				approvals: [],
			},
		});
		mock.methods["turn/start"] = (params) => {
			if (params.threadId !== "chat") throw Error("Wrong chat");
			counts.start++;
			running = true;
			const turn = { id: "turn", status: "inProgress" };
			mock.emitHost({ method: "turn/started", params: { threadId: "chat", turn } }, "desktop");
			return { turn };
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
	await page.getByRole("textbox", { name: "Message ReCodex" }).fill("Run once");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(page.getByRole("button", { name: "Stop generation" })).toBeVisible();
	await page.evaluate(() => (window as unknown as { disconnectPc: () => void }).disconnectPc());
	await expect(page.getByRole("button", { name: "Connect", exact: true })).toBeVisible();
	await page.evaluate(() => (window as unknown as { announcePc: () => void }).announcePc());
	await expect(page.getByRole("button", { name: "Connect", exact: true })).toHaveCount(0);
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
