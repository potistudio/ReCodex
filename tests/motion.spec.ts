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
	const popoverBorder = await popover.evaluate((element) => {
		const styles = getComputedStyle(element);
		return { outer: styles.borderTopColor, shadows: styles.boxShadow };
	});
	expect(popoverBorder.outer).toBe("rgba(255, 255, 255, 0.5)");
	expect(popoverBorder.shadows).toContain("rgba(0, 0, 0, 0.5) 0px 0px 0px 1px inset");
	const modelBMedium = page.getByRole("button", { name: "Model B, Medium" });
	await modelBMedium.click();
	await expect(modelBMedium).toHaveAttribute("aria-pressed", "true");
	await page.keyboard.press("Escape");
	await trigger.press("Enter");
	await expect(popover).toBeVisible();
	await expect(page.locator(".model-effort-popover")).toHaveCount(1);
	await page.keyboard.press("Escape");
	await expect(popover).toBeHidden();
	await expect(trigger).toBeFocused();
});

test("grid cells switch model and effort and reset to the model default", async ({ page }) => {
	await page.locator(".model-effort-trigger").click();
	const modelBUltra = page.getByRole("button", { name: "Model B, Ultra" });

	await modelBUltra.click();
	await expect(modelBUltra).toHaveAttribute("aria-pressed", "true");
	await expect(page.locator(".model-effort-current")).toContainText("Model B");
	await expect(page.locator(".model-effort-current")).toContainText("Ultra");

	await page.getByRole("button", { name: "Reset reasoning effort" }).click();
	await expect(page.getByRole("button", { name: "Model B, High" })).toHaveAttribute("aria-pressed", "true");
});

test("selection labels roll in the direction of the selected position", async ({ page }) => {
	await page.addStyleTag({ content: ":root { --motion-normal: 1000ms; }" });
	await page.locator(".model-effort-trigger").click();

	async function sampleRoll(selector: string) {
		return page.locator(selector).evaluate((element) => {
			const labels = [...element.querySelectorAll<HTMLElement>(":scope > span")];
			if (labels.length !== 2) throw new Error("The outgoing and incoming labels are missing");
			return labels.map((label) => {
				const animation = label.getAnimations().find((entry) => entry.effect instanceof KeyframeEffect);
				if (!animation?.effect) throw new Error("The label is not rolling");
				animation.pause();
				animation.currentTime = Number(animation.effect.getTiming().duration) / 2;
				return {
					label: label.textContent,
					y: new DOMMatrixReadOnly(getComputedStyle(label).transform).m42,
				};
			});
		});
	}

	await page.getByRole("button", { name: "Model A, Extra high" }).click();
	const raisedEffort = await sampleRoll(".effort-label-copy");
	expect(raisedEffort.find(({ label }) => label === "Extra high")?.y).toBeGreaterThan(0);

	await page.waitForTimeout(200);
	await page.getByRole("button", { name: "Model B, Extra high" }).click();
	const raisedModel = await sampleRoll(".model-effort-name-copy");
	expect(raisedModel.find(({ label }) => label === "Model B")?.y).toBeLessThan(0);
});

test("grid cells are keyboard-operable and keep the composer label in sync", async ({ page }) => {
	const trigger = page.locator(".model-effort-trigger");
	await expect(trigger).toHaveAccessibleName("Model A · Medium");
	const initialWidth = await trigger.evaluate((element) => element.getBoundingClientRect().width);
	await trigger.click();
	await expect(trigger).toHaveAccessibleName("Selecting model...");

	const modelAUltra = page.getByRole("button", { name: "Model A, Ultra" });
	await modelAUltra.focus();
	await modelAUltra.press("Enter");
	await expect(modelAUltra).toHaveAttribute("aria-pressed", "true");

	await page.keyboard.press("Escape");
	await expect(trigger).toHaveAccessibleName("Model A · Ultra");
	await expect
		.poll(() => trigger.evaluate((element) => element.getBoundingClientRect().width))
		.not.toBeCloseTo(initialWidth, 1);
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
	const modelBUltra = page.getByRole("button", { name: "Model B, Ultra" });
	await modelBUltra.click();
	await expect(modelBUltra).toHaveAttribute("aria-pressed", "true");
	await page.keyboard.press("Escape");
	await expect(popover).toHaveCount(0);
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect(page.locator(".sidebar")).toHaveCount(0);
});
