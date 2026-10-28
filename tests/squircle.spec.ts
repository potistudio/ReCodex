import { expect, test } from "@playwright/test";

test("squircle fallback preserves interactive controls and theme colors", async ({ page }) => {
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
	await expect
		.poll(() =>
			page
				.locator(".composer")
				.evaluate((element) =>
					decodeURIComponent((element as HTMLElement).style.getPropertyValue("--hyperellipse-layer-image")),
				),
		)
		.toContain('fill="rgb(255, 255, 255)"');
	await account.click();
	const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
	await expect(dialog).toBeVisible();
	await expect.poll(() => dialog.evaluate((element) => getComputedStyle(element).clipPath)).toMatch(/^path\(/);
	const selected = page.locator(".theme-option.selected");
	await expect(selected).toHaveAttribute("data-hyperellipse-host", /layer/);
	await expect
		.poll(() =>
			selected.evaluate((element) =>
				decodeURIComponent((element as HTMLElement).style.getPropertyValue("--hyperellipse-layer-image")),
			),
		)
		.toContain('fill="rgb(255, 255, 255)"');
	await page.getByRole("radio", { name: "ReCodex Dark", exact: true }).check();
	await expect(page.locator("html")).toHaveClass("dark");
	await dialog.getByRole("button", { name: "Close" }).click();
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
	const inner = page.locator(".workspace-body");
	for (const element of [outer, inner]) {
		await expect.poll(() => element.evaluate((node) => getComputedStyle(node).clipPath)).toMatch(/^path\(/);
	}
	const borders = await outer.evaluate((element) => {
		const inner = element.querySelector<HTMLElement>(".workspace-body");
		if (!inner) throw new Error("Workspace content is missing");
		const outerRect = element.getBoundingClientRect();
		const innerRect = inner.getBoundingClientRect();
		return {
			inset: innerRect.left - outerRect.left,
			borderWidth: getComputedStyle(element).borderTopWidth,
			outerBackground: getComputedStyle(element).backgroundColor,
			innerBackground: getComputedStyle(inner).backgroundColor,
		};
	});
	expect(borders.inset).toBe(2);
	expect(borders.borderWidth).toBe("1px");
	expect(borders.outerBackground).not.toBe(borders.innerBackground);
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
});
