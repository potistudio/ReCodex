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
