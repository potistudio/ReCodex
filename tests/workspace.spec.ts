import { expect, test } from "@playwright/test";
import { installTauriMock, type TauriMock } from "./support/tauri-mock";

const hideCssStudio = "css-studio-panel { display: none !important; }";

test("browser preview and connection controls", async ({ page }) => {
	await page.goto("/");
	await page.addStyleTag({ content: hideCssStudio });
	await expect(page.getByRole("textbox", { name: "Message ReCodex" })).toBeVisible();
	await expect(page.getByRole("combobox", { name: "Active host" })).toHaveValue("local");
	await expect(page.getByText("Browser preview ·", { exact: false })).toBeVisible();
	await page.getByRole("textbox", { name: "Message ReCodex" }).fill("Give me a concise overview");
	await expect(page.getByRole("button", { name: "Send message", exact: true })).toBeDisabled();
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await expect(page.getByRole("dialog", { name: "Settings", exact: true })).toBeVisible();
});

test("desktop IPC flow: project, model, streaming approval, file save, history", async ({ page }) => {
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	await installTauriMock(page);
	await page.addInitScript(() => {
		const host = window as unknown as Record<string, unknown>;
		const mock = host.tauriMock as TauriMock;
		let turnCount = 0;
		let file = "export const answer = 41;\n";
		let projects = [{ name: "Example project", path: "C:/example", historyPaths: [] as string[] }];
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
		const emit = (payload: unknown) => mock.emit("host-event", payload);
		mock.commands.projects_load = () => projects;
		mock.commands.project_save = (args) => {
			projects = projects.map((project) => ({
				...project,
				name: (args.name as string | undefined) ?? project.name,
			}));
			return projects;
		};
		mock.commands.project_relocate = (args) => {
			host.relocateParams = args;
			const sourcePath = (args.sourcePath as string) ?? "";
			const destinationPath = (args.destinationPath as string) ?? "";
			const project = projects.find((entry) => entry.path === sourcePath);
			if (!project) throw new Error("Open this project first");
			project.historyPaths.push(project.path);
			project.path = destinationPath;
			project.historyPaths = project.historyPaths.filter((path) => path !== destinationPath);
			projects = projects.filter(
				(entry, index, entries) => index === entries.findIndex((item) => item.path === entry.path),
			);
			return project;
		};
		mock.commands.files_list = () => [{ name: "main.ts", path: "main.ts", isDirectory: false }];
		mock.commands.file_read = () => file;
		mock.commands.file_save = (args) => {
			if (args.expected !== file) throw new Error("File changed on disk");
			file = args.content as string;
		};
		mock.commands["plugin:dialog|confirm"] = () => true;
		mock.commands["plugin:dialog|open"] = () => "D:/relocated-working-directory";
		mock.commands.server_respond = (args) => {
			host.approvalResult = args.result;
			emit({
				method: "thread/tokenUsage/updated",
				params: {
					threadId: thread.id,
					turnId: "turn-1",
					tokenUsage: {
						total: {
							totalTokens: 1000,
							inputTokens: 600,
							cachedInputTokens: 400,
							cacheWriteInputTokens: 0,
							outputTokens: 200,
							reasoningOutputTokens: 200,
						},
						last: {
							totalTokens: 1000,
							inputTokens: 600,
							cachedInputTokens: 400,
							cacheWriteInputTokens: 0,
							outputTokens: 200,
							reasoningOutputTokens: 200,
						},
						modelContextWindow: 272000,
					},
				},
			});
			emit({
				method: "item/completed",
				params: {
					threadId: thread.id,
					turnId: "turn-1",
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
					turnId: "turn-1",
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
		};
		mock.methods["model/list"] = () => ({
			data: [
				{
					id: "m1",
					model: "model-a",
					displayName: "Model A",
					description: "Fast model",
					isDefault: true,
					defaultReasoningEffort: "medium",
					supportedReasoningEfforts: [
						{ reasoningEffort: "low", description: "Faster responses" },
						{ reasoningEffort: "medium", description: "Balanced reasoning" },
					],
				},
				{
					id: "m2",
					model: "model-b",
					displayName: "Model B",
					description: "Reasoning model",
					isDefault: false,
					defaultReasoningEffort: "high",
					supportedReasoningEfforts: [
						{ reasoningEffort: "medium", description: "Balanced reasoning" },
						{ reasoningEffort: "high", description: "Deeper reasoning" },
					],
				},
			],
			nextCursor: null,
		});
		mock.methods["thread/list"] = (params) => {
			host.threadListCwds = [...((host.threadListCwds as string[] | undefined) ?? []), params.cwd as string];
			return { data: [thread, backgroundThread], nextCursor: null };
		};
		mock.methods["thread/start"] = async (params) => {
			host.startParams = params;
			if (host.delayThreadStart) await new Promise<void>((resolve) => (host.releaseThreadStart = resolve));
			return { thread };
		};
		mock.methods["thread/resume"] = (params) => {
			host.resumeThreadIds = [
				...((host.resumeThreadIds as string[] | undefined) ?? []),
				params.threadId as string,
			];
			const resumedThread = params.threadId === backgroundThread.id ? backgroundThread : thread;
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
		};
		mock.methods["turn/start"] = (params) => {
			turnCount += 1;
			const turnId = `turn-${turnCount}`;
			host.turnParams = params;
			host.turnInputs = [...((host.turnInputs as unknown[] | undefined) ?? []), params];
			setTimeout(() => {
				emit({
					method: "turn/started",
					params: { threadId: thread.id, turn: { id: turnId, status: "inProgress", items: [] } },
				});
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
							content: (params as { input: unknown }).input,
						},
					},
				});
				emit({
					method: "item/reasoning/summaryTextDelta",
					params: {
						threadId: thread.id,
						itemId: "reasoning-1",
						summaryIndex: 0,
						delta: "**Reviewing ",
					},
				});
				setTimeout(() => {
					emit({
						method: "item/reasoning/summaryTextDelta",
						params: {
							threadId: thread.id,
							itemId: "reasoning-1",
							summaryIndex: 0,
							delta: "the project**",
						},
					});
					setTimeout(() => {
						emit({
							method: "item/reasoning/summaryTextDelta",
							params: {
								threadId: thread.id,
								itemId: "reasoning-1",
								summaryIndex: 1,
								delta: "**Assessing ",
							},
						});
						emit({
							method: "item/reasoning/summaryTextDelta",
							params: {
								threadId: thread.id,
								itemId: "reasoning-1",
								summaryIndex: 1,
								delta: "dependencies**",
							},
						});
						setTimeout(() => {
							emit({
								method: "item/agentMessage/delta",
								params: {
									threadId: thread.id,
									itemId: "answer-1",
									turnId,
									delta: "This is a **",
								},
							});
							emit({
								method: "item/agentMessage/delta",
								params: {
									threadId: thread.id,
									itemId: "answer-1",
									turnId,
									delta: "SvelteKit**",
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
						}, 1000);
					}, 50);
				}, 50);
			}, 20);
			return {
				turn: { id: turnId, status: "inProgress", items: [] },
			};
		};
	});
	await page.goto("/");
	await page.addStyleTag({ content: `${hideCssStudio} .messages { padding-top: 1200px !important; }` });
	await expect(page.getByText("Connected to This PC", { exact: true })).toBeVisible();
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await page.getByRole("button", { name: "Show sidebar" }).click();
	const modelEffortTrigger = page.locator(".composer .model-effort-trigger");
	await expect(modelEffortTrigger).toHaveAccessibleName("Model A · Medium");
	await modelEffortTrigger.click();
	await expect(modelEffortTrigger).toHaveAccessibleName("Selecting model...");
	await expect(modelEffortTrigger.locator(".model-effort-selecting")).toBeVisible();
	await expect(page.getByRole("dialog", { name: "Model and reasoning effort" })).toBeVisible();
	await expect(page.locator(".model-effort-summary")).toContainText("Model A");
	await page.getByRole("button", { name: "Choose model", exact: true }).click();
	await expect(page.getByRole("button", { name: /Model A/ })).toHaveAttribute("aria-pressed", "true");
	await page.getByRole("button", { name: /Model B/ }).click();
	await expect(modelEffortTrigger).toHaveAccessibleName("Selecting model...");
	await expect(page.locator(".model-effort-summary")).toContainText("Model B");
	const effortSlider = page.getByRole("slider", { name: "Reasoning effort", exact: true });
	await expect(effortSlider).toHaveAttribute("aria-valuetext", "High");
	await effortSlider.focus();
	await effortSlider.press("ArrowLeft");
	await expect(modelEffortTrigger).toHaveAccessibleName("Selecting model...");
	await page.getByRole("button", { name: "Reset reasoning effort", exact: true }).click();
	await expect(effortSlider).toHaveAttribute("aria-valuetext", "High");
	await effortSlider.fill("0");
	await expect(effortSlider).toHaveAttribute("aria-valuetext", "Medium");
	await effortSlider.press("Escape");
	await expect(page.getByRole("dialog", { name: "Model and reasoning effort" })).toBeHidden();
	await expect(modelEffortTrigger).toHaveAccessibleName("Model B · Medium");
	await expect(modelEffortTrigger.locator(".model-effort-selecting")).toBeHidden();
	await expect(modelEffortTrigger).toBeFocused();
	const composer = page.getByRole("textbox", { name: "Message ReCodex" });
	await composer.fill("日本語の確定");
	await composer.dispatchEvent("compositionstart");
	// Some engines do not expose isComposing on the confirmation key.
	await composer.dispatchEvent("keydown", { key: "Enter", isComposing: false });
	await expect(composer).toHaveValue("日本語の確定");
	await page.getByRole("button", { name: "Send message", exact: true }).click();
	await expect(composer).toHaveValue("日本語の確定");
	await composer.dispatchEvent("compositionend");
	// WebKit can emit compositionend before the confirming keydown.
	await composer.dispatchEvent("keydown", { key: "Enter", keyCode: 229, isComposing: false });
	await expect(composer).toHaveValue("日本語の確定");
	await composer.dispatchEvent("keydown", { key: "Enter", isComposing: true });
	await expect(composer).toHaveValue("日本語の確定");
	expect(await page.evaluate(() => (window as unknown as { turnInputs?: unknown[] }).turnInputs)).toBeUndefined();
	await composer.fill("Explain this project");
	await composer.press("Shift+Enter");
	await expect(composer).toHaveValue("Explain this project\n");
	await page.evaluate(() => ((window as unknown as { delayThreadStart: boolean }).delayThreadStart = true));
	await composer.press("Enter");
	await expect(page.locator(".user-message > .message-sent")).toBeVisible();
	await expect(page.getByRole("button", { name: "Scroll to latest" })).toBeHidden();
	await expect
		.poll(() =>
			page.evaluate(() => typeof (window as unknown as { releaseThreadStart?: unknown }).releaseThreadStart),
		)
		.toBe("function");
	await page.evaluate(() => (window as unknown as { releaseThreadStart: () => void }).releaseThreadStart());
	await expect
		.poll(async () => (await page.locator(".working-label-token-incoming").allTextContents()).join("").trimEnd())
		.toBe("Reviewing");
	expect((await page.locator(".working-label-token-outgoing").allTextContents()).join("").trimEnd()).toBe("Thinking");
	expect(
		await page
			.locator(".working-label-token-incoming")
			.first()
			.evaluate((element) => getComputedStyle(element).animationName.includes("working-label-push-in")),
	).toBe(true);
	await expect(page.getByRole("heading", { name: "Permission requested" })).toBeVisible();
	await expect
		.poll(() =>
			page.locator(".chat-scroll-frame").evaluate((frame) => {
				const approval = frame.querySelector(".approval-card");
				const blur = frame.querySelector(".chat-edge-blur-bottom");
				if (!approval || !blur) return false;
				const gap = blur.getBoundingClientRect().top - approval.getBoundingClientRect().bottom;
				return gap >= 0 && gap <= 32;
			}),
		)
		.toBe(true);
	await expect(page.locator(".streaming-markdown p")).toHaveText("This is a SvelteKit");
	await expect(page.locator(".assistant-message").getByText(/estimated/)).toBeVisible();
	await expect(page.locator(".streaming-markdown strong")).toHaveText("SvelteKit");
	await expect(page.locator(".streaming-markdown strong .message-append")).toHaveText("SvelteKit");
	expect(
		await page
			.locator(".streaming-markdown strong .message-append")
			.evaluate((element) => getComputedStyle(element).animationName),
	).toBe("message-append");
	await page.getByRole("button", { name: "Background conversation", exact: true }).click();
	await expect(page.locator(".markdown")).toContainText("Background conversation");
	const historyHeight = await page.locator(".chat-scroll").evaluate((element) => {
		const height = element.scrollHeight;
		element.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -100 }));
		element.scrollTop -= 200;
		return height;
	});
	await expect(page.getByRole("button", { name: "Scroll to latest" })).toBeVisible();
	expect(await page.locator(".chat-scroll").evaluate((element) => element.scrollHeight)).toBe(historyHeight);
	await expect(page.getByRole("button", { name: "New chat" })).toBeEnabled();
	await page.getByRole("button", { name: "Explain this project", exact: true }).click();
	await expect(page.getByRole("heading", { name: "Permission requested" })).toBeVisible();
	expect(await page.evaluate(() => (window as unknown as { resumeThreadIds: string[] }).resumeThreadIds)).toEqual([
		"thread-2",
	]);
	await expect(page.locator(".tool-item.running")).toContainText("Running");
	await expect(page.locator(".tool-item.running pre")).toContainText("Checking types…");
	await expect(page.getByRole("button", { name: "Decline", exact: true })).toBeVisible();
	const readingScrollTop = await page.locator(".chat-scroll").evaluate((element) => {
		const message = element.querySelector<HTMLElement>(".user-message");
		if (!message) throw new Error("User message not found");
		element.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -100 }));
		element.scrollTop += message.getBoundingClientRect().top - element.getBoundingClientRect().top - 200;
		return element.scrollTop;
	});
	await expect
		.poll(() =>
			page.locator(".chat-scroll").evaluate((element) => {
				const message = element.querySelector<HTMLElement>(".user-message");
				return message ? message.getBoundingClientRect().top - element.getBoundingClientRect().top : null;
			}),
		)
		.toBeCloseTo(200, 0);
	await page.getByRole("button", { name: "Ask for another approach" }).click();
	await expect(page.locator(".tool-item")).toContainText("Completed");
	await expect(page.getByText("This is a SvelteKit project.", { exact: true })).toBeVisible();
	await expect
		.poll(() =>
			page.locator(".chat-scroll").evaluate((element, top) => {
				const expected = Math.min(top, element.scrollHeight - element.clientHeight);
				return Math.abs(element.scrollTop - expected);
			}, readingScrollTop),
		)
		.toBeLessThanOrEqual(1);
	await expect(page.getByRole("button", { name: "Scroll to latest" })).toBeVisible();
	await page.getByRole("button", { name: "Scroll to latest" }).click();
	await expect(page.getByRole("button", { name: "Scroll to latest" })).toBeHidden();
	const remainingScroll = () =>
		page
			.locator(".chat-scroll")
			.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight);
	await expect.poll(remainingScroll).toBeLessThan(1);
	await page.setViewportSize({ width: 1280, height: 700 });
	await expect.poll(remainingScroll).toBeLessThan(1);
	await page.setViewportSize({ width: 1280, height: 860 });
	await expect.poll(remainingScroll).toBeLessThan(1);
	await expect(page.locator(".thread-list").getByText("1,000 tokens", { exact: true })).toBeVisible();
	await expect(page.locator(".assistant-message").getByText(/1,000 tokens/)).toBeVisible();
	await expect(page.locator(".assistant-message").getByText(/stream average/)).toBeVisible();
	await page.locator(".account-button").click();
	await expect(page.getByText("Model providers", { exact: true })).toBeVisible();
	await expect(page.getByText("Command Code account", { exact: true })).toBeVisible();
	await expect(page.getByRole("button", { name: "Sign in", exact: true })).toBeEnabled();
	await page.getByRole("button", { name: "Close" }).click();
	await expect(page.locator(".user-message")).toHaveCount(2);
	expect(
		await page.evaluate(
			() =>
				(window as unknown as { turnParams: { effort: string; model: string; summary: string } }).turnParams
					.model,
		),
	).toBe("model-b");
	expect(
		await page.evaluate(
			() =>
				(window as unknown as { turnParams: { effort: string; model: string; summary: string } }).turnParams
					.effort,
		),
	).toBe("medium");
	expect(
		await page.evaluate(
			() => (window as unknown as { turnParams: { model: string; summary: string } }).turnParams.summary,
		),
	).toBe("concise");
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
	await page.evaluate(() => ((window as unknown as { threadListCwds: string[] }).threadListCwds = []));
	await page.getByRole("button", { name: "Project options" }).click();
	await page.getByRole("menuitem", { name: "Change working directory" }).click();
	expect(await page.evaluate(() => (window as unknown as { relocateParams: unknown }).relocateParams)).toEqual({
		sourcePath: "C:/example",
		destinationPath: "D:/relocated-working-directory",
	});
	expect(await page.evaluate(() => (window as unknown as { threadListCwds: string[] }).threadListCwds)).toEqual([
		"D:/relocated-working-directory",
		"C:/example",
	]);
	await expect(page.getByRole("button", { name: /^Explain this project/ })).toBeVisible();
	await page.getByRole("button", { name: /New chat/ }).click();
	await page.getByRole("button", { name: /^Explain this project/ }).click();
	await expect(page.getByText("Restored conversation", { exact: true })).toBeVisible();
	expect(errors).toEqual([]);
});
