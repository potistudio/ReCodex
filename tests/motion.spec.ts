import { expect, type Locator, test } from "@playwright/test";

test.use({ reducedMotion: "no-preference" });

test.beforeEach(async ({ page }) => {
	await page.addInitScript(() => {
		const host = window as unknown as Record<string, unknown>;
		host.isTauri = true;
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

test("hover, dialogs, and nested popovers animate and restore focus", async ({ page }) => {
	const suggestion = page.getByRole("button", { name: "Explore the code" });
	await suggestion.hover();
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { motionEvents: string[] }).motionEvents))
		.toContain("background-color");
	const account = page.getByRole("button", { name: /Your workspace/ });
	await account.click();
	const settings = page.getByRole("dialog", { name: "Settings", exact: true });
	await sampleEntrance(settings);
	await page.keyboard.press("Escape");
	await expect(settings).toBeHidden();
	await expect(account).toBeFocused();
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { motionEvents: string[] }).motionEvents))
		.toContain("surface-out");

	const trigger = page.locator(".model-effort-trigger");
	await trigger.click();
	const popover = page.getByRole("dialog", { name: "Model and reasoning effort" });
	await sampleEntrance(popover);
	const chooseModel = page.getByRole("button", { name: "Choose model", exact: true });
	await chooseModel.click();
	await sampleEntrance(page.locator(".model-picker-menu"));
	await page.keyboard.press("Escape");
	await expect(page.locator(".model-picker-menu")).toBeHidden();
	await expect(popover).toBeVisible();
	await expect(chooseModel).toBeFocused();
	await page.keyboard.press("Escape");
	await trigger.press("Enter");
	await expect(popover).toBeVisible();
	await expect(page.locator(".model-effort-popover")).toHaveCount(1);
	await page.keyboard.press("Escape");
	await expect(popover).toBeHidden();
	await expect(trigger).toBeFocused();
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

test("dragging effort keeps the composer label and slider anchored", async ({ page }) => {
	const trigger = page.locator(".model-effort-trigger");
	await trigger.click();
	const slider = page.getByRole("slider", { name: "Reasoning effort" });
	await slider.press("Home");
	await expect(trigger).toHaveAccessibleName("Model A · Low");
	await expect.poll(() => trigger.evaluate((element) => element.getAnimations({ subtree: true }).length)).toBe(0);
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
	await expect(trigger).toHaveAccessibleName("Model A · Low");
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
	expect(offsets[1]).toBeGreaterThan(0);
	expect(offsets[1]).toBeLessThan(interval * 0.02);
	expect(offsets[3]).toBeCloseTo(interval / 2, 1);
	expect(offsets[5]).toBeGreaterThan(interval * 0.98);
	expect(offsets[5]).toBeLessThan(interval);
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

test("panels reverse cleanly and reduced motion keeps controls immediate", async ({ page }) => {
	const contentWidth = await page
		.locator(".sidebar > .panel-content")
		.evaluate((element) => element.getBoundingClientRect().width);
	await page.getByRole("button", { name: "Hide sidebar" }).click();
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
