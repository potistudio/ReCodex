import { expect, test } from "@playwright/test";

test("all theme previews apply their palette across the workspace", async ({ page }) => {
	await page.goto("/");
	await page.getByRole("textbox", { name: "Message Codex" }).fill("Keep this draft while switching themes.");
	await page.getByRole("button", { name: /Your workspace/ }).click();
	const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
	await expect(dialog.getByRole("radio")).toHaveCount(22);

	const palettes = [
		["ReCodex Light", "light", "#fcfcfa", "light"],
		["ReCodex Dark", "dark", "#20221f", "dark"],
		["Linear Light", "linear-light", "#ffffff", "light"],
		["Linear Dark", "linear-dark", "#101114", "dark"],
		["OpenAI Light", "openai-light", "#ffffff", "light"],
		["OpenAI Dark", "openai-dark", "#212121", "dark"],
		["Catppuccin Latte", "catppuccin-latte", "#eff1f5", "light"],
		["Catppuccin Frappé", "catppuccin-frappe", "#303446", "dark"],
		["Catppuccin Macchiato", "catppuccin-macchiato", "#24273a", "dark"],
		["Catppuccin Mocha", "catppuccin-mocha", "#1e1e2e", "dark"],
		["One Dark", "one-dark", "#282c34", "dark"],
		["One Light", "one-light", "#fafafa", "light"],
		["Tokyo Night", "tokyo-night", "#1a1b26", "dark"],
		["Tokyo Night Storm", "tokyo-night-storm", "#24283b", "dark"],
		["Tokyo Night Moon", "tokyo-night-moon", "#222436", "dark"],
		["Tokyo Night Day", "tokyo-night-day", "#e1e2e7", "light"],
		["Nord", "nord", "#2e3440", "dark"],
		["Dracula", "dracula", "#282a36", "dark"],
		["Gruvbox Dark", "gruvbox-dark", "#282828", "dark"],
		["Gruvbox Light", "gruvbox-light", "#fbf1c7", "light"],
		["Rosé Pine", "rose-pine", "#191724", "dark"],
		["Rosé Pine Dawn", "rose-pine-dawn", "#faf4ed", "light"],
	];

	for (const [name, id, background, mode] of palettes) {
		await test.step(name, async () => {
			const radio = dialog.getByRole("radio", { name, exact: true });
			await radio.check();
			await expect(radio).toBeChecked();
			await expect(dialog.locator(".theme-option.selected")).toHaveCount(1);
			await expect(dialog.locator(".theme-selection")).toHaveText(`Current theme: ${name}`);
			await expect(page.locator("html")).toHaveAttribute("data-theme", id);
			await expect(page.locator("html")).toHaveCSS("color-scheme", mode);
			const rgb = background
				.slice(1)
				.match(/../g)
				?.map((part) => Number.parseInt(part, 16));
			await expect(page.locator("body")).toHaveCSS("background-color", `rgb(${rgb?.join(", ")})`);
			if (mode === "dark") await expect(page.locator("html")).toHaveClass("dark");
			else await expect(page.locator("html")).not.toHaveClass("dark");

			const contrast = await page.evaluate(() => {
				const root = getComputedStyle(document.documentElement);
				const value = (name: string) => root.getPropertyValue(`--${name}`).trim();
				const luminance = (hex: string) => {
					const [r, g, b] = (hex.slice(1).match(/../g) ?? []).map((part) => {
						const channel = Number.parseInt(part, 16) / 255;
						return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
					});
					return r * 0.2126 + g * 0.7152 + b * 0.0722;
				};
				return [
					["foreground", "background"],
					["foreground", "sidebar"],
					["foreground", "popover"],
					["foreground", "accent"],
					["muted-foreground", "popover"],
					["muted-foreground", "muted"],
					["primary-foreground", "primary"],
				].map(([foreground, surface]) => {
					const first = luminance(value(foreground));
					const second = luminance(value(surface));
					return {
						pair: `${foreground}/${surface}`,
						ratio: (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05),
					};
				});
			});
			for (const { pair, ratio } of contrast) expect(ratio, `${name}: ${pair}`).toBeGreaterThanOrEqual(4.5);

			const colors = await page.locator("html").evaluate((element) => {
				const style = getComputedStyle(element);
				return {
					sidebar: style.getPropertyValue("--sidebar").trim(),
					popover: style.getPropertyValue("--popover").trim(),
					foreground: style.getPropertyValue("--foreground").trim(),
				};
			});
			const toRgb = (hex: string) =>
				`rgb(${hex
					.slice(1)
					.match(/../g)
					?.map((part) => Number.parseInt(part, 16))
					.join(", ")})`;
			await expect(page.locator(".sidebar")).toHaveCSS("background-color", toRgb(colors.sidebar));
			await expect(dialog).toHaveCSS("background-color", toRgb(colors.popover));
			await expect(dialog).toHaveCSS("color", toRgb(colors.foreground));
		});
	}
	await dialog.getByRole("button", { name: "Close", exact: true }).click();
	await expect(page.getByRole("textbox", { name: "Message Codex" })).toHaveValue(
		"Keep this draft while switching themes.",
	);
});

test("theme selection is restored after closing settings and reloading", async ({ page }) => {
	await page.goto("/");
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await page.getByRole("radio", { name: "Catppuccin Mocha", exact: true }).check();
	await expect.poll(() => page.evaluate(() => localStorage.getItem("recodex-theme"))).toBe("catppuccin-mocha");
	await page.getByRole("button", { name: "Close", exact: true }).click();
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await expect(page.getByRole("radio", { name: "Catppuccin Mocha", exact: true })).toBeChecked();
	await page.reload();
	await expect(page.locator("body")).toHaveCSS("background-color", "rgb(30, 30, 46)");
	await expect(page.locator("html")).toHaveClass("dark");
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await expect(page.getByRole("radio", { name: "Catppuccin Mocha", exact: true })).toBeChecked();
	await page.getByRole("radio", { name: "OpenAI Light", exact: true }).check();
	await page.reload();
	await expect(page.locator("html")).toHaveAttribute("data-theme", "openai-light");
	await expect(page.locator("html")).not.toHaveClass("dark");
});

for (const [saved, expected] of [
	["light", "light"],
	["dark", "dark"],
	["removed-theme", "light"],
]) {
	test(`restores saved preference ${saved}`, async ({ page }) => {
		await page.addInitScript((theme) => localStorage.setItem("recodex-theme", theme), saved);
		await page.goto("/");
		await expect(page.locator("html")).toHaveAttribute("data-theme", expected);
		await expect(page.locator("html")).toHaveCSS("color-scheme", expected);
	});
}

test("theme picker supports keyboard selection in a small window", async ({ page }) => {
	await page.setViewportSize({ width: 760, height: 600 });
	await page.goto("/");
	await page.getByRole("button", { name: /Your workspace/ }).click();
	const radio = page.getByRole("radio", { name: "ReCodex Light", exact: true });
	await radio.focus();
	await page.keyboard.press("ArrowRight");
	await expect(page.getByRole("radio", { name: "ReCodex Dark", exact: true })).toBeChecked();
	await expect(page.locator("html")).toHaveClass("dark");
	await page.getByRole("radio", { name: "Rosé Pine Dawn", exact: true }).check();
	await expect(page.locator("html")).toHaveAttribute("data-theme", "rose-pine-dawn");
	const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
	const bounds = await dialog.boundingBox();
	expect(bounds).not.toBeNull();
	if (bounds) {
		expect(bounds.y).toBeGreaterThanOrEqual(0);
		expect(bounds.y + bounds.height).toBeLessThanOrEqual(600);
	}
	expect(await dialog.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
	await page.keyboard.press("Escape");
	await expect(dialog).not.toBeVisible();
});

test("themes remain usable when persistent storage is unavailable", async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, "localStorage", {
			get() {
				throw new DOMException("Storage is unavailable", "SecurityError");
			},
		});
	});
	await page.goto("/");
	await expect(page.getByRole("heading", { name: "What will you build?" })).toBeVisible();
	await page.getByRole("button", { name: /Your workspace/ }).click();
	await page.getByRole("radio", { name: "Tokyo Night", exact: true }).check();
	await expect(page.locator("body")).toHaveCSS("background-color", "rgb(26, 27, 38)");
});
