import { expect, type Locator, test } from "@playwright/test";

test.use({ reducedMotion: "no-preference" });

test.beforeEach(async ({ page }) => {
	await page.addInitScript(() => {
		const host = window as unknown as Record<string, unknown>;
		host.isTauri = true;
		host.__TAURI_OS_PLUGIN_INTERNALS__ = { platform: "macos" };
		host.motionEvents = [];
		document.addEventListener("animationstart", (event) => {
			(host.motionEvents as string[]).push(event.animationName);
		});
		document.addEventListener("transitionrun", (event) => {
			(host.motionEvents as string[]).push(event.propertyName);
		});
		host.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
		host.__TAURI_INTERNALS__ = {
			transformCallback() {
				return 1;
			},
			async invoke(command: string, args: Record<string, unknown> = {}) {
				if (command === "plugin:event|listen") return 1;
				if (command === "plugin:event|unlisten" || command === "server_connect") return;
				if (command === "projects_load")
					return [{ name: "Motion preview", path: "C:/example", historyPaths: [] }];
				if (command === "files_list") return [];
				if (command !== "server_request") throw new Error(`Unexpected command: ${command}`);
				if (args.method === "model/list")
					return {
						data: [
							{
								id: "model-a",
								model: "model-a",
								displayName: "Model A",
								description: "Preview model",
								isDefault: true,
								hidden: false,
								defaultReasoningEffort: "medium",
								supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map(
									(reasoningEffort) => ({ reasoningEffort, description: reasoningEffort }),
								),
							},
							{
								id: "model-b",
								model: "model-b",
								displayName: "Model B",
								description: "Alternative preview model",
								isDefault: false,
								hidden: false,
								defaultReasoningEffort: "high",
								supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map(
									(reasoningEffort) => ({ reasoningEffort, description: reasoningEffort }),
								),
							},
						],
						nextCursor: null,
					};
				if (args.method === "account/read") return { account: null, requiresOpenaiAuth: false };
				if (args.method === "account/rateLimits/read") return null;
				if (args.method === "thread/list") return { data: [], nextCursor: null };
				throw new Error(`Unexpected method: ${args.method}`);
			},
		};
	});
	await page.goto("/");
	await expect(page.locator(".model-effort-trigger")).toBeEnabled();
});

async function sampleEntrance(surface: Locator) {
	const sample = await surface.evaluate((element) => {
		const animation = element
			.getAnimations()
			.find((entry) => entry instanceof CSSAnimation && entry.animationName === "surface-in");
		if (!animation) throw new Error("The surface has no entrance animation");
		animation.pause();
		animation.currentTime = Number(animation.effect?.getTiming().duration) / 2;
		const opacity = Number(getComputedStyle(element).opacity);
		animation.finish();
		return opacity;
	});
	expect(sample).toBeGreaterThan(0);
	expect(sample).toBeLessThan(1);
}

test("settings dialog animates and restores focus", async ({ page }) => {
	const account = page.getByRole("button", { name: /Your workspace/ });
	await account.click();
	const settings = page.getByRole("dialog", { name: "Settings", exact: true });
	await sampleEntrance(settings);
	await page.keyboard.press("Escape");
	await expect(settings).toBeHidden();
	await expect(account).toBeFocused();
});

test("the workspace fills the space below the title bar at any header height", async ({ page }) => {
	const header = page.locator(".topbar");
	const shell = page.locator(".app-shell");
	for (const height of [44, 72]) {
		await header.evaluate((element, value) => {
			(element as HTMLElement).style.height = `${value}px`;
		}, height);
		const bounds = await shell.boundingBox();
		expect(bounds).not.toBeNull();
		if (!bounds) throw new Error("Missing workspace bounds");
		expect(bounds.y).toBe(height);
		expect(bounds.y + bounds.height).toBe(860);
	}
});

test("the whole title bar can be dragged without blocking its controls", async ({ page }) => {
	const header = page.locator(".topbar");
	const hits = await header.evaluate((element) => {
		const bounds = element.getBoundingClientRect();
		return [2, bounds.width / 2, bounds.width - 2].map((x) =>
			document.elementFromPoint(bounds.left + x, bounds.top + 2)?.hasAttribute("data-tauri-drag-region"),
		);
	});
	expect(hits).toEqual([true, true, true]);
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect(page.getByRole("button", { name: "Show sidebar" })).toBeVisible();
	await page.getByRole("button", { name: "Files", exact: true }).click();
	await expect(page.getByRole("complementary", { name: "Project files" })).toBeVisible();
});

test("the title bar stays fixed while the sidebar changes width", async ({ page }) => {
	const header = page.locator(".topbar");
	const button = page.getByRole("button", { name: "Hide sidebar" });
	const before = await button.boundingBox();
	const headerBounds = await header.boundingBox();
	const sidebarBounds = await page.locator(".sidebar").boundingBox();
	const mainBounds = await page.locator(".main-workspace").boundingBox();
	expect(before).not.toBeNull();
	expect(headerBounds).not.toBeNull();
	expect(sidebarBounds).not.toBeNull();
	expect(mainBounds).not.toBeNull();
	if (!before || !headerBounds || !sidebarBounds || !mainBounds) throw new Error("Missing layout bounds");
	expect(headerBounds.height).toBeGreaterThan(0);
	expect(sidebarBounds.y).toBe(headerBounds.y + headerBounds.height);
	expect(mainBounds.y).toBe(sidebarBounds.y);
	expect(mainBounds.x).toBeGreaterThanOrEqual(sidebarBounds.x + sidebarBounds.width);
	await button.click();
	const during = await page.getByRole("button", { name: "Show sidebar" }).boundingBox();
	expect(during?.x).toBe(before.x);
	expect(during?.y).toBe(before.y);
	await expect(page.locator(".sidebar")).toHaveCount(0);
	const after = await page.getByRole("button", { name: "Show sidebar" }).boundingBox();
	expect(after?.x).toBe(before.x);
	expect(after?.y).toBe(before.y);
	await expect
		.poll(async () => (await page.locator(".main-workspace").boundingBox())?.width ?? 0)
		.toBeGreaterThan(mainBounds.width);
});

test("panels reverse cleanly and reduced motion keeps controls immediate", async ({ page }) => {
	const contentWidth = await page
		.locator(".sidebar > .panel-content")
		.evaluate((element) => element.getBoundingClientRect().width);
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
	const movingContentWidth = await page.locator(".sidebar").evaluate((element) => {
		const animation = element
			.getAnimations()
			.find(
				(entry) =>
					entry.effect instanceof KeyframeEffect &&
					entry.effect.getKeyframes().some((frame) => "width" in frame),
			);
		if (!animation) throw new Error("The panel has no width animation");
		animation.pause();
		animation.currentTime = Number(animation.effect?.getTiming().duration) / 2;
		const width = element.querySelector(".panel-content")?.getBoundingClientRect().width;
		animation.play();
		return width;
	});
	expect(movingContentWidth).toBeCloseTo(contentWidth, 0);
	await page.getByRole("button", { name: "Show sidebar" }).press("Enter");
	await expect(page.locator(".sidebar")).toHaveCount(1);
	await expect(page.locator(".sidebar")).toHaveCSS("width", "256px");
	await page.getByRole("button", { name: "Search chats", exact: false }).click();
	await expect(page.getByRole("textbox", { name: "Search chats" })).toBeVisible();
	await page.getByRole("button", { name: "Search chats", exact: false }).click();
	await expect(page.getByRole("textbox", { name: "Search chats" })).toBeHidden();
	await page.getByRole("button", { name: "Files", exact: true }).click();
	await expect(page.getByRole("complementary", { name: "Project files" })).toBeVisible();
	await page.getByRole("button", { name: "Close files" }).click();
	await expect(page.getByRole("complementary", { name: "Project files" })).toBeHidden();

	await page.emulateMedia({ reducedMotion: "reduce" });
	await page.locator(".model-effort-trigger").click();
	const menu = page.getByRole("menu");
	await expect(menu).toHaveCSS("animation-name", "none");
	await page.getByRole("menuitemradio", { name: /Model B/ }).click();
	await expect(page.locator(".model-effort-trigger")).toHaveAccessibleName("Model B · High");
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect(page.locator(".sidebar")).toHaveCount(0);
});
