import { expect, test } from "@playwright/test";
import { installTauriMock, type TauriMock } from "./support/tauri-mock";

test("deletes a chat only after confirmation and preserves it on failure", async ({ page }) => {
	await installTauriMock(page);
	await page.addInitScript(() => {
		const host = window as unknown as Record<string, unknown>;
		const mock = host.tauriMock as TauriMock;
		host.confirmDelete = false;
		host.failDelete = false;
		host.deleteRequests = [];
		let threads = [
			{ id: "one", name: "First chat", preview: "First chat", cwd: "/project", updatedAt: 2, turns: [] },
			{ id: "two", name: "Second chat", preview: "Second chat", cwd: "/project", updatedAt: 1, turns: [] },
		];
		const emit = (payload: unknown) => mock.emit("host-event", payload);
		host.emitTestEvent = emit;
		mock.commands.projects_load = () => [{ name: "Project", path: "/project", historyPaths: [] }];
		mock.commands["plugin:dialog|message"] = (args) => {
			host.confirmText = args.message;
			return host.confirmDelete ? "Delete" : "Cancel";
		};
		mock.methods["model/list"] = () => ({ data: [], nextCursor: null });
		mock.methods["thread/list"] = () => {
			if (host.deferThreadList) {
				const snapshot = [...threads];
				return new Promise((resolve) => {
					host.resolveThreadList = () => resolve({ data: snapshot, nextCursor: null });
				});
			}
			return { data: threads, nextCursor: null };
		};
		mock.methods["thread/resume"] = (params) => {
			const thread = threads.find((entry) => entry.id === params.threadId);
			return { thread, model: "", reasoningEffort: null };
		};
		mock.methods["thread/delete"] = async (params) => {
			(host.deleteRequests as unknown[]).push(params.threadId);
			if (host.failDelete) throw new Error("Delete failed");
			const complete = () => {
				threads = threads.filter((thread) => thread.id !== params.threadId);
				emit({ method: "thread/deleted", params: { threadId: params.threadId } });
			};
			if (host.deferDelete) {
				await new Promise<void>((resolve) => {
					host.resolveDelete = () => {
						complete();
						resolve();
					};
				});
			} else complete();
			return {};
		};
	});
	await page.goto("/");
	await page.getByRole("button", { name: "First chat", exact: true }).click();
	const first = page.getByRole("button", { name: "First chat", exact: true });
	await expect(first).toHaveClass(/selected/);
	const deleteFirst = page.getByRole("button", { name: "Delete chat: First chat" });
	await deleteFirst.focus();
	await deleteFirst.click();
	expect(await page.evaluate(() => (window as unknown as { deleteRequests: string[] }).deleteRequests)).toEqual([]);
	await expect(first).toBeVisible();
	expect(await page.evaluate(() => (window as unknown as { confirmText: string }).confirmText)).toContain(
		"First chat” and any chats it spawned? This cannot be undone.",
	);
	await page.evaluate(() => {
		const host = window as unknown as Record<string, unknown>;
		host.confirmDelete = true;
		host.failDelete = true;
	});
	await deleteFirst.click();
	await expect(page.getByRole("alert")).toContainText("Delete failed");
	await expect(first).toBeVisible();
	await expect(first).toHaveClass(/selected/);
	await page.evaluate(() => {
		const host = window as unknown as Record<string, unknown>;
		host.failDelete = false;
		host.deferDelete = true;
	});
	await deleteFirst.click();
	await expect(first).toBeDisabled();
	await expect
		.poll(() => page.evaluate(() => typeof (window as unknown as Record<string, unknown>).resolveDelete))
		.toBe("function");
	await page.evaluate(() => (window as unknown as { resolveDelete: () => void }).resolveDelete());
	await expect(first).toHaveCount(0);
	await expect(page.getByRole("button", { name: "Second chat", exact: true })).toBeVisible();
	expect(await page.evaluate(() => (window as unknown as { deleteRequests: string[] }).deleteRequests)).toEqual([
		"one",
		"one",
	]);
	await page.evaluate(() => {
		const host = window as unknown as { emitTestEvent: (event: unknown) => void };
		host.emitTestEvent({
			method: "turn/started",
			params: { threadId: "two", turn: { id: "turn", items: [], status: "inProgress" } },
		});
	});
	await expect(page.getByRole("button", { name: "Delete chat: Second chat" })).toBeDisabled();
	await page.evaluate(() => {
		const host = window as unknown as { deferThreadList: boolean; emitTestEvent: (event: unknown) => void };
		host.deferThreadList = true;
		host.emitTestEvent({
			method: "turn/completed",
			params: { threadId: "two", turn: { id: "turn", items: [], status: "completed" } },
		});
	});
	await expect
		.poll(() => page.evaluate(() => typeof (window as unknown as Record<string, unknown>).resolveThreadList))
		.toBe("function");
	await page.evaluate(() => {
		const host = window as unknown as { emitTestEvent: (event: unknown) => void; resolveThreadList: () => void };
		host.emitTestEvent({ method: "thread/deleted", params: { threadId: "two" } });
		host.resolveThreadList();
	});
	await expect(page.getByRole("button", { name: "Second chat", exact: true })).toHaveCount(0);
});
