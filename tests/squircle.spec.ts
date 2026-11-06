import { expect, test } from "@playwright/test";

test("squircle fallback preserves interactive controls", async ({ page }) => {
	await page.addInitScript(() => {
		const supports = CSS.supports.bind(CSS);
		CSS.supports = ((property: string, value?: string) => {
			if (property === "corner-shape" && value === "squircle") return false;
			return value === undefined ? supports(property) : supports(property, value);
		}) as typeof CSS.supports;
	});
	await page.addInitScript(() => {
		document.addEventListener("DOMContentLoaded", () => {
			const style = document.createElement("style");
			style.textContent =
				'[data-slot="dialog-content"] { box-shadow: none !important; } [data-hyperellipse-host~="layer"] { transition-property: color, opacity, transform, translate, scale, width, height, left, right, top, bottom, margin, padding, filter !important; }';
			document.head.appendChild(style);
		});
	});
	await page.goto("/");
	const account = page.getByRole("button", { name: /Your workspace/ });
	await expect.poll(() => account.evaluate((element) => getComputedStyle(element).clipPath)).toMatch(/^path\(/);
	await expect(page.locator(".composer")).toHaveAttribute("data-hyperellipse-host", /layer/);
	await page.waitForTimeout(2000);
	// The composer is painted with `--card`; read the color from the token so a palette change does not break this.
	const card = await page.evaluate(() => {
		const probe = document.createElement("div");
		probe.style.background = "var(--card)";
		document.body.append(probe);
		const color = getComputedStyle(probe).backgroundColor;
		probe.remove();
		return color;
	});
	await expect
		.poll(() =>
			page
				.locator(".composer")
				.evaluate((element) =>
					decodeURIComponent((element as HTMLElement).style.getPropertyValue("--hyperellipse-layer-image")),
				),
		)
		.toContain(`fill="${card}"`);
	await account.click();
	const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
	await expect(dialog).toBeVisible();
	await expect.poll(() => dialog.evaluate((element) => getComputedStyle(element).clipPath)).toMatch(/^path\(/);
	const close = dialog.getByRole("button", { name: "Close" });
	// The dialog focuses its first field, not Close. Reaching Close by keyboard gives it a focus ring, and the ring is
	// what switches the button to layer mode.
	await close.focus();
	await page.keyboard.press("Tab");
	await page.keyboard.press("Shift+Tab");
	await expect(close).toBeFocused();
	await expect(close).toHaveAttribute("data-hyperellipse-host", /layer/);
	await expect
		.poll(() =>
			close.evaluate((element) =>
				decodeURIComponent((element as HTMLElement).style.getPropertyValue("--hyperellipse-layer-image")),
			),
		)
		.toContain("<svg");
	await close.click();
	await expect(dialog).toBeHidden();
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect(page.getByRole("button", { name: "Show sidebar" })).toBeVisible();
});

test("workspace fallback keeps its border and clips content while resizing", async ({ page }) => {
	await page.addInitScript(() => {
		const supports = CSS.supports.bind(CSS);
		CSS.supports = ((property: string, value?: string) => {
			if (property === "corner-shape" && value === "squircle") return false;
			return value === undefined ? supports(property) : supports(property, value);
		}) as typeof CSS.supports;
	});
	await page.goto("/");
	const outer = page.locator(".main-workspace");
	const rim = page.locator(".workspace-rim");
	const inner = page.locator(".workspace-body");
	await expect(page.locator(".workspace-shadow")).toHaveCSS("filter", /drop-shadow/);
	await expect(outer).toHaveCSS("filter", "none");
	for (const element of [outer, rim, inner]) {
		await expect.poll(() => element.evaluate((node) => getComputedStyle(node).clipPath)).toMatch(/^path\(/);
	}
	const borders = await outer.evaluate((element) => {
		const inner = element.querySelector<HTMLElement>(".workspace-body");
		const rim = element.querySelector<HTMLElement>(".workspace-rim");
		if (!inner || !rim) throw new Error("Workspace content is missing");
		const outerRect = element.getBoundingClientRect();
		const innerRect = inner.getBoundingClientRect();
		return {
			inset: innerRect.left - outerRect.left,
			padding: getComputedStyle(element).paddingTop,
			rimBackground: getComputedStyle(rim).backgroundColor,
			image: getComputedStyle(element).backgroundImage,
			outerBackground: getComputedStyle(element).backgroundColor,
			innerBackground: getComputedStyle(inner).backgroundColor,
		};
	});
	expect(borders.inset).toBe(2);
	expect(borders.padding).toBe("1px");
	expect(borders.image).toBe("none");
	expect(borders.outerBackground).not.toBe(borders.rimBackground);
	expect(borders.rimBackground).not.toBe(borders.innerBackground);
	const hit = await inner.evaluate((element) => {
		const testContent = document.createElement("div");
		testContent.style.cssText = "position:absolute;inset:0;z-index:999;background:red";
		element.append(testContent);
		const rect = element.getBoundingClientRect();
		const corner = document.elementFromPoint(rect.left + 0.5, rect.top + 0.5) === testContent;
		const center =
			document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2) === testContent;
		testContent.remove();
		return { corner, center };
	});
	expect(hit).toEqual({ corner: false, center: true });
	const before = await outer.boundingBox();
	await page.getByRole("button", { name: "Hide sidebar" }).click();
	await expect.poll(async () => (await outer.boundingBox())?.width ?? 0).toBeGreaterThan(before?.width ?? 0);
	await expect.poll(() => inner.evaluate((node) => getComputedStyle(node).clipPath)).toMatch(/^path\(/);
	const samples = await page.evaluate(async () => {
		const frames: string[] = [];
		let running = true;
		function sample() {
			const outer = document.querySelector(".main-workspace");
			if (outer) frames.push(getComputedStyle(outer).backgroundImage);
			if (running) requestAnimationFrame(sample);
		}
		requestAnimationFrame(sample);
		for (let i = 0; i < 16; i++) {
			document
				.querySelector<HTMLButtonElement>('[aria-label="Hide sidebar"], [aria-label="Show sidebar"]')
				?.click();
			await new Promise((resolve) => setTimeout(resolve, 75));
		}
		running = false;
		return frames;
	});
	expect(samples.length).toBeGreaterThan(10);
	expect(samples.every((image) => image === "none")).toBe(true);
});
