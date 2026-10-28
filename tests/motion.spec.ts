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

test("model list morphs in place and returns to the slider", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();
	const dialog = page.getByRole("dialog", { name: "Model and reasoning effort" });
	const choose = page.getByRole("button", { name: "Choose model" });
	const slider = page.getByRole("slider", { name: "Reasoning effort" });
	const views = page.locator(".model-picker-views");
	await dialog.evaluate((element) => Promise.all(element.getAnimations().map((animation) => animation.finished)));
	const before = await views.boundingBox();
	expect(before?.height).toBe(28);
	await choose.click();
	await expect(choose).toHaveAttribute("aria-expanded", "true");
	await expect(page.locator(".model-picker-effort")).toHaveJSProperty("inert", true);
	const modelB = page.getByRole("button", { name: /Model B/ });
	await expect
		.poll(() =>
			views.evaluate((element) =>
				element
					.getAnimations()
					.some(
						(animation) => animation instanceof CSSTransition && animation.transitionProperty === "height",
					),
			),
		)
		.toBe(true);
	await expect(modelB).toBeVisible();
	await expect(page.locator(".model-picker-effort")).toHaveCSS("opacity", "0");
	const after = await views.boundingBox();
	expect(after?.x).toBe(before?.x);
	expect(after?.height).toBeGreaterThan(28);
	await page.keyboard.press("Escape");
	await expect(dialog).toBeVisible();
	await expect(slider).toBeVisible();
	await expect.poll(async () => (await views.boundingBox())?.height).toBe(28);
	await expect(choose).toBeFocused();
	await choose.click();
	await modelB.click();
	await expect(slider).toHaveAttribute("aria-valuetext", "High");
	await expect(choose).toHaveAttribute("aria-expanded", "false");
	await expect(choose).toBeFocused();
});

test("effort fill and thumb update together without position transitions", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();
	const slider = page.getByRole("slider", { name: "Reasoning effort" });
	const thumb = page.locator(".effort-thumb");
	const start = await thumb.evaluate((element) => Number.parseFloat(getComputedStyle(element).left));
	await slider.fill("5");
	const sample = await page.locator(".effort-control").evaluate((element) => {
		const thumb = element.querySelector(".effort-thumb");
		const fill = element.querySelector(".effort-fill");
		if (!thumb || !fill) throw new Error("Effort visuals are missing");
		const positionTransitions = [thumb, fill]
			.flatMap((node) => node.getAnimations())
			.filter(
				(animation) =>
					animation instanceof CSSTransition && ["left", "width"].includes(animation.transitionProperty),
			);
		return {
			left: Number.parseFloat(getComputedStyle(thumb).left),
			width: Number.parseFloat(getComputedStyle(fill).width),
			positionTransitions: positionTransitions.length,
		};
	});
	expect(sample.left).toBeCloseTo(202, 1);
	expect(sample.width - sample.left).toBeCloseTo(14, 1);
	expect(sample.positionTransitions).toBe(0);
	await slider.press("Home");
	await slider.press("End");
	await slider.press("ArrowLeft");
	await expect(slider).toHaveAttribute("aria-valuetext", "Max");
	await expect
		.poll(() => thumb.evaluate((element) => Number.parseFloat(getComputedStyle(element).left)))
		.toBeCloseTo(161.6, 0);
	await page.getByRole("button", { name: "Reset reasoning effort" }).click();
	await expect(slider).toHaveAttribute("aria-valuetext", "Medium");
	await expect
		.poll(() => thumb.evaluate((element) => Number.parseFloat(getComputedStyle(element).left)))
		.toBeCloseTo(start, 0);
});

test("effort labels roll in the direction of the selected grade", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();
	const slider = page.getByRole("slider", { name: "Reasoning effort" });

	async function sampleRoll() {
		return page.locator(".effort-label-copy").evaluate((element) => {
			const labels = [...element.querySelectorAll<HTMLElement>(".effort-label-value")];
			if (labels.length !== 2) throw new Error("The outgoing and incoming effort labels are missing");
			return labels.map((label) => {
				const animation = label.getAnimations().find((entry) => entry.effect instanceof KeyframeEffect);
				if (!animation?.effect) throw new Error("The effort label is not rolling");
				animation.pause();
				animation.currentTime = Number(animation.effect.getTiming().duration) / 2;
				return {
					label: label.textContent,
					y: new DOMMatrixReadOnly(getComputedStyle(label).transform).m42,
				};
			});
		});
	}

	await slider.fill("3");
	const raised = await sampleRoll();
	const raisedTo = raised.find(({ label }) => label === "Extra high");
	expect(raisedTo?.y).toBeGreaterThan(0);

	await page.waitForTimeout(200);
	await slider.fill("1");
	const lowered = await sampleRoll();
	const loweredTo = lowered.find(({ label }) => label === "Medium");
	expect(loweredTo?.y).toBeLessThan(0);
});

test("model labels roll in the direction of the selected model", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();

	async function sampleRoll() {
		return page.locator(".model-effort-name-copy").evaluate((element) => {
			const labels = [...element.querySelectorAll<HTMLElement>(".model-effort-name")];
			if (labels.length !== 2) throw new Error("The outgoing and incoming model labels are missing");
			return labels.map((label) => {
				const animation = label.getAnimations().find((entry) => entry.effect instanceof KeyframeEffect);
				if (!animation?.effect) throw new Error("The model label is not rolling");
				animation.pause();
				animation.currentTime = Number(animation.effect.getTiming().duration) / 2;
				return {
					label: label.textContent,
					y: new DOMMatrixReadOnly(getComputedStyle(label).transform).m42,
				};
			});
		});
	}

	const chooseModel = page.getByRole("button", { name: "Choose model", exact: true });
	await chooseModel.click();
	await page.getByRole("button", { name: /Model B/ }).click();
	const raised = await sampleRoll();
	expect(raised.find(({ label }) => label === "Model B")?.y).toBeLessThan(0);

	await page.waitForTimeout(200);
	await chooseModel.click();
	await page.getByRole("button", { name: /Model A/ }).click();
	const lowered = await sampleRoll();
	expect(lowered.find(({ label }) => label === "Model A")?.y).toBeGreaterThan(0);
});

test("the composer keeps its width while selecting and resizes on confirmation", async ({ page }) => {
	const trigger = page.locator(".model-effort-trigger");
	await expect(trigger).toHaveAccessibleName("Model A · Medium");
	await expect.poll(() => trigger.evaluate((element) => element.getAnimations({ subtree: true }).length)).toBe(0);
	const initialWidth = await trigger.evaluate((element) => element.getBoundingClientRect().width);
	await page.evaluate(() => {
		(window as unknown as { motionEvents: string[] }).motionEvents = [];
	});
	await trigger.click();
	const slider = page.getByRole("slider", { name: "Reasoning effort" });
	await slider.press("Home");
	await expect(trigger).toHaveAccessibleName("Selecting model...");
	await expect.poll(() => trigger.evaluate((element) => element.getAnimations({ subtree: true }).length)).toBe(0);
	const selectingGeometry = await trigger.evaluate((element) => {
		const copy = element.querySelector(".model-effort-trigger-copy")?.getBoundingClientRect();
		const label = element.querySelector(".model-effort-selecting")?.getBoundingClientRect();
		if (!copy || !label) throw new Error("The selecting label is missing");
		return {
			width: element.getBoundingClientRect().width,
			centerOffset: label.x + label.width / 2 - (copy.x + copy.width / 2),
		};
	});
	expect(selectingGeometry.width).toBeCloseTo(initialWidth, 1);
	expect(selectingGeometry.centerOffset).toBeCloseTo(0, 1);
	expect(await page.evaluate(() => (window as unknown as { motionEvents: string[] }).motionEvents)).not.toContain(
		"width",
	);
	const bounds = await slider.boundingBox();
	if (!bounds) throw new Error("The effort slider is missing");
	await page.evaluate(() => {
		const host = window as unknown as {
			effortGeometry: { width: number; popoverX: number; sliderX: number }[];
			stopEffortSampling: () => void;
		};
		const trigger = document.querySelector(".model-effort-trigger");
		const popover = document.querySelector(".model-effort-popover");
		const slider = document.querySelector(".effort-slider");
		if (!trigger || !popover || !slider) throw new Error("The effort picker is missing");
		host.effortGeometry = [];
		let frame = 0;
		const sample = () => {
			host.effortGeometry.push({
				width: trigger.getBoundingClientRect().width,
				popoverX: popover.getBoundingClientRect().x,
				sliderX: slider.getBoundingClientRect().x,
			});
			frame = requestAnimationFrame(sample);
		};
		sample();
		host.stopEffortSampling = () => cancelAnimationFrame(frame);
	});
	const y = bounds.y + bounds.height / 2;
	await page.mouse.move(bounds.x + 14, y);
	await page.mouse.down();
	await page.mouse.move(bounds.x + bounds.width - 14, y, { steps: 36 });
	await expect(slider).toHaveAttribute("aria-valuetext", "Ultra");
	await page.mouse.move(bounds.x + 14, y, { steps: 36 });
	await page.mouse.up();
	await expect(slider).toHaveAttribute("aria-valuetext", "Low");
	await expect(trigger).toHaveAccessibleName("Selecting model...");
	await expect.poll(() => trigger.evaluate((element) => element.getAnimations({ subtree: true }).length)).toBe(0);
	const samples = await page.evaluate(() => {
		const host = window as unknown as {
			effortGeometry: { width: number; popoverX: number; sliderX: number }[];
			stopEffortSampling: () => void;
		};
		host.stopEffortSampling();
		return host.effortGeometry;
	});
	expect(samples.length).toBeGreaterThan(10);
	for (const coordinate of ["width", "popoverX", "sliderX"] as const) {
		const values = samples.map((sample) => sample[coordinate]);
		expect(Math.max(...values) - Math.min(...values), coordinate).toBeLessThan(1);
	}
	await slider.press("Escape");
	await expect(trigger).toHaveAccessibleName("Model A · Low");
	await expect
		.poll(() => trigger.evaluate((element) => element.getBoundingClientRect().width))
		.toBeLessThan(initialWidth - 10);
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { motionEvents: string[] }).motionEvents))
		.toContain("width");
});

test("effort positions flatten near steps and depend on position rather than time or direction", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();
	const slider = page.getByRole("slider", { name: "Reasoning effort" });
	const thumb = page.locator(".effort-thumb");
	await expect
		.poll(() =>
			page
				.locator(".model-effort-popover")
				.evaluate(
					(element) =>
						element.getAnimations().filter((animation) => animation.playState !== "finished").length,
				),
		)
		.toBe(0);
	await expect.poll(() => thumb.evaluate((element) => element.getAnimations().length)).toBe(0);
	const initial = await thumb.boundingBox();
	if (!initial) throw new Error("The effort thumb is missing");
	const x = initial.x + initial.width / 2;
	const y = initial.y + initial.height / 2;
	const sliderBounds = await slider.boundingBox();
	if (!sliderBounds) throw new Error("The effort slider is missing");
	const interval = (sliderBounds.width - initial.width) / Number(await slider.getAttribute("max"));
	await page.mouse.move(x, y);
	await page.mouse.down();
	const fractions = [0, 0.1, 0.25, 0.5, 0.75, 0.9, 1];
	const offsets: number[] = [];
	for (const fraction of fractions) {
		await page.mouse.move(x + interval * fraction, y);
		const bounds = await thumb.boundingBox();
		if (!bounds) throw new Error("The effort thumb disappeared");
		expect(bounds.width).toBeCloseTo(initial.width, 1);
		expect(bounds.height).toBeCloseTo(initial.height, 1);
		offsets.push(bounds.x - initial.x);
	}
	expect(offsets[0]).toBeCloseTo(0, 1);
	expect(offsets[1]).toBeCloseTo(0, 1);
	expect(offsets[2]).toBeGreaterThan(0);
	expect(offsets[2]).toBeLessThan(interval * 0.01);
	expect(offsets[3]).toBeCloseTo(interval / 2, 0);
	expect(offsets[4]).toBeGreaterThan(interval * 0.99);
	expect(offsets[4]).toBeLessThan(interval);
	expect(offsets[5]).toBeCloseTo(interval, 1);
	expect(offsets[6]).toBeCloseTo(interval, 1);
	for (let index = fractions.length - 1; index >= 0; index--) {
		await page.mouse.move(x + interval * fractions[index], y, { steps: 5 });
		await page.waitForTimeout(50);
		const offset = await thumb.evaluate((element, origin) => element.getBoundingClientRect().x - origin, initial.x);
		expect(offset).toBeCloseTo(offsets[index], 1);
	}
	await page.mouse.up();
	await expect(slider).toHaveAttribute("aria-valuetext", "Medium");
	const released = await thumb.boundingBox();
	expect(released?.x).toBeCloseTo(initial.x, 1);
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
	const popover = page.getByRole("dialog", { name: "Model and reasoning effort" });
	await expect(popover).toHaveCSS("animation-name", "none");
	await page.getByRole("slider", { name: "Reasoning effort" }).fill("5");
	await expect(page.locator(".effort-thumb")).toHaveCSS("transition-duration", "0s");
	await expect(page.locator(".effort-thumb")).toHaveCSS("left", "202px");
	await page.keyboard.press("Escape");
	await expect(popover).toHaveCount(0);
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect(page.locator(".sidebar")).toHaveCount(0);
});
