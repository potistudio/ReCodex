import { expect, test } from "@playwright/test";

test("browser preview, suggestions, and theme", async ({ page }) => {
	await page.goto("/");
	await expect(page.getByRole("heading", { name: "What will you build?" })).toBeVisible();
	await expect(page.getByText("Browser preview ·", { exact: false })).toBeVisible();
	await page.getByRole("button", { name: "Explore the code" }).click();
	await expect(page.getByRole("textbox", { name: "Message Codex" })).toHaveValue(/Give me a concise overview/);
	await expect(page.getByRole("button", { name: "Send message", exact: true })).toBeDisabled();
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await page.getByRole("button", { name: "Dark", exact: true }).click();
	await expect(page.locator("html")).toHaveClass("dark");
});

test("desktop IPC flow: project, model, streaming approval, file save, history", async ({ page }) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await page.addInitScript(() => {
		type Payload = Record<string, unknown>;
		const host = window as unknown as Payload;
		host.isTauri = true;
		const callbacks = new Map<number, (event: unknown) => void>();
		let listener = 0;
		let callbackId = 0;
		let turnCount = 0;
		let file = "export const answer = 41;\n";
		let projects = [{ name: "Example project", path: "C:/example" }];
		const thread = {
			id: "thread-1",
			name: null,
			preview: "Explain this project",
			cwd: "C:/example",
			updatedAt: 1,
			turns: [],
		};
		const backgroundThread = {
			id: "thread-2",
			name: "Background conversation",
			preview: "Background conversation",
			cwd: "C:/example",
			updatedAt: 0,
			turns: [],
		};
		const emit = (payload: unknown) => callbacks.get(listener)?.({ event: "codex-event", payload });
		host.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
		host.__TAURI_INTERNALS__ = {
			transformCallback(callback: (event: unknown) => void) {
				callbacks.set(++callbackId, callback);
				return callbackId;
			},
			async invoke(command: string, args: Payload = {}) {
				if (command === "plugin:event|listen") {
					listener = args.handler as number;
					return 1;
				}
				if (command === "plugin:event|unlisten") return;
				if (command === "server_connect") return;
				if (command === "projects_load") return projects;
				if (command === "project_save") {
					projects = projects.map((project) => ({
						...project,
						name: (args.name as string | undefined) ?? project.name,
					}));
					return projects;
				}
				if (command === "files_list")
					return [
						{
							name: "main.ts",
							path: "main.ts",
							isDirectory: false,
						},
					];
				if (command === "file_read") return file;
				if (command === "file_save") {
					if (args.expected !== file) throw new Error("File changed on disk");
					file = args.content as string;
					return;
				}
				if (command === "plugin:dialog|confirm") return true;
				if (command === "server_respond") {
					host.approvalResult = args.result;
					emit({
						method: "item/completed",
						params: {
							threadId: thread.id,
							item: {
								id: "command-1",
								type: "commandExecution",
								command: "pnpm check",
								status: "completed",
							},
						},
					});
					emit({
						method: "item/completed",
						params: {
							threadId: thread.id,
							item: {
								id: "answer-1",
								type: "agentMessage",
								text: "This is a **SvelteKit** project.",
							},
						},
					});
					emit({
						method: "turn/completed",
						params: {
							threadId: thread.id,
							turn: {
								id: "turn-1",
								status: "completed",
								items: [],
							},
						},
					});
					return;
				}
				if (command !== "server_request") throw new Error(`Unexpected command: ${command}`);
				if (args.method === "model/list")
					return {
						data: [
							{
								id: "m1",
								model: "model-a",
								displayName: "Model A",
								description: "Fast model",
								isDefault: true,
								defaultReasoningEffort: "medium",
								supportedReasoningEfforts: [{ reasoningEffort: "medium" }],
							},
							{
								id: "m2",
								model: "model-b",
								displayName: "Model B",
								description: "Reasoning model",
								isDefault: false,
								defaultReasoningEffort: "high",
								supportedReasoningEfforts: [{ reasoningEffort: "high" }],
							},
						],
						nextCursor: null,
					};
				if (args.method === "account/read")
					return {
						account: {
							type: "chatgpt",
							email: "test@example.com",
							planType: "plus",
						},
						requiresOpenaiAuth: true,
					};
				if (args.method === "thread/list") return { data: [thread, backgroundThread], nextCursor: null };
				if (args.method === "thread/start") {
					host.startParams = args.params;
					return { thread };
				}
				if (args.method === "thread/resume") {
					host.resumeThreadIds = [
						...((host.resumeThreadIds as string[] | undefined) ?? []),
						(args.params as { threadId: string }).threadId,
					];
					const resumedThread =
						(args.params as { threadId: string }).threadId === backgroundThread.id
							? backgroundThread
							: thread;
					return {
						thread: {
							...resumedThread,
							turns: [
								{
									id: "old-turn",
									status: "completed",
									items: [
										{
											id: "old-answer",
											type: "agentMessage",
											text:
												resumedThread.id === backgroundThread.id
													? "Background conversation"
													: "Restored conversation",
										},
									],
								},
							],
						},
						model: "model-a",
					};
				}
				if (args.method === "turn/start") {
					turnCount += 1;
					const turnId = `turn-${turnCount}`;
					host.turnParams = args.params;
					host.turnInputs = [...((host.turnInputs as unknown[] | undefined) ?? []), args.params];
					setTimeout(() => {
						if (turnId !== "turn-1") {
							emit({
								method: "turn/completed",
								params: {
									threadId: thread.id,
									turn: { id: turnId, status: "completed", items: [] },
								},
							});
							return;
						}
						emit({
							method: "item/started",
							params: {
								threadId: thread.id,
								item: {
									id: "user-1",
									type: "userMessage",
									content: (args.params as { input: unknown }).input,
								},
							},
						});
						emit({
							method: "item/agentMessage/delta",
							params: {
								threadId: thread.id,
								itemId: "answer-1",
								delta: "This is a ",
							},
						});
						emit({
							method: "item/started",
							params: {
								threadId: thread.id,
								item: {
									id: "command-1",
									type: "commandExecution",
									command: "pnpm check",
									status: "inProgress",
								},
							},
						});
						emit({
							method: "item/commandExecution/outputDelta",
							params: {
								threadId: thread.id,
								itemId: "command-1",
								delta: "Checking types…\n",
							},
						});
						emit({
							id: 42,
							method: "item/commandExecution/requestApproval",
							params: {
								threadId: thread.id,
								turnId,
								command: "pnpm check",
								reason: "Validate the project",
								availableDecisions: ["accept"],
							},
						});
					}, 20);
					return {
						turn: { id: turnId, status: "inProgress", items: [] },
					};
				}
				throw new Error(`Unexpected method: ${args.method}`);
			},
		};
	});
	await page.goto("/");
	await expect(page.getByText("Codex connected", { exact: true })).toBeVisible();
	await page.getByRole("button", { name: "Model A", exact: true }).click();
	await page.getByRole("menuitem", { name: /Model B/ }).click();
	await page.getByRole("textbox", { name: "Message Codex" }).fill("Explain this project");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(page.getByRole("heading", { name: "Permission requested" })).toBeVisible();
	await page.getByRole("button", { name: "Background conversation", exact: true }).click();
	await expect(page.locator(".markdown")).toContainText("Background conversation");
	await expect(page.getByRole("button", { name: "New chat" })).toBeEnabled();
	await page.getByRole("button", { name: "Explain this project", exact: true }).click();
	await expect(page.getByRole("heading", { name: "Permission requested" })).toBeVisible();
	expect(await page.evaluate(() => (window as unknown as { resumeThreadIds: string[] }).resumeThreadIds)).toEqual([
		"thread-2",
	]);
	await expect(page.locator(".tool-item.running")).toContainText("Running");
	await expect(page.locator(".tool-item.running pre")).toContainText("Checking types…");
	await expect(page.getByRole("button", { name: "Decline", exact: true })).toBeVisible();
	await page.getByRole("button", { name: "Ask for another approach" }).click();
	await expect(page.locator(".tool-item")).toContainText("Completed");
	await expect(page.getByText("This is a SvelteKit project.", { exact: true })).toBeVisible();
	await expect(page.locator(".user-message")).toHaveCount(2);
	expect(await page.evaluate(() => (window as unknown as { turnParams: { model: string } }).turnParams.model)).toBe(
		"model-b",
	);
	expect(await page.evaluate(() => (window as unknown as { approvalResult: unknown }).approvalResult)).toEqual({
		decision: "decline",
	});
	await expect
		.poll(() =>
			page.evaluate(
				() => (window as unknown as { turnInputs?: { input?: { text?: string }[] }[] }).turnInputs?.length ?? 0,
			),
		)
		.toBe(2);
	expect(
		await page.evaluate(
			() => (window as unknown as { turnInputs: { input: { text?: string }[] }[] }).turnInputs[1].input[0].text,
		),
	).toContain("The user declined the previous permission request");
	await page.getByRole("button", { name: "Files", exact: true }).click();
	await page.getByRole("button", { name: "main.ts", exact: true }).click();
	await page.getByRole("textbox", { name: "Edit main.ts" }).fill("export const answer = 42;\n");
	await page.getByRole("button", { name: "Save", exact: true }).click();
	await expect(page.getByText("Saved", { exact: true })).toBeVisible();
	await page.getByRole("button", { name: "Reload file from disk" }).click();
	await expect(page.getByRole("textbox", { name: "Edit main.ts" })).toHaveValue("export const answer = 42;\n");
	await page.getByRole("button", { name: "Project options" }).click();
	await page.getByRole("menuitem", { name: "Rename project" }).click();
	await page.getByRole("textbox", { name: "Project name" }).fill("Renamed project");
	await page.getByRole("button", { name: "Save name" }).click();
	await expect(page.getByRole("button", { name: "Renamed project", exact: true })).toBeVisible();
	await page.getByRole("button", { name: /New chat/ }).click();
	await page.getByRole("button", { name: "Explain this project", exact: true }).click();
	await expect(page.getByText("Restored conversation", { exact: true })).toBeVisible();
	expect(errors).toEqual([]);
});
