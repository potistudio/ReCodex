import { expect, test } from "@playwright/test";

test("streamed Markdown preserves animation progress across reparsing and completion", async ({ page }) => {
	await page.goto("/");
	await page.evaluate(async () => {
		const path = "/src/lib/streaming-markdown.ts";
		const { streamingMarkdown } = await import(/* @vite-ignore */ path);
		const node = document.createElement("div");
		node.id = "streaming-test";
		document.body.append(node);
		const action = streamingMarkdown(node, { html: "<p>Hello</p>", streaming: true });
		Object.assign(window, { streamingTest: action });
	});

	await page.waitForTimeout(100);
	await page.evaluate(() => {
		const action = (window as unknown as { streamingTest: { update(state: unknown): void } }).streamingTest;
		action.update({ html: "<p><strong>Hello world</strong></p>", streaming: true });
	});
	const oldText = page.locator("#streaming-test strong .message-append").first();
	const newText = page.locator("#streaming-test strong .message-append").last();
	await expect(oldText).toHaveText("Hello");
	await expect(newText).toHaveText(" world");
	const oldDelay = await oldText.evaluate((element) =>
		Number.parseFloat((element as HTMLElement).style.animationDelay),
	);
	const newDelay = await newText.evaluate((element) =>
		Number.parseFloat((element as HTMLElement).style.animationDelay),
	);
	expect(oldDelay).toBeLessThan(-60);
	expect(newDelay).toBeGreaterThan(-10);

	await page.evaluate(() => {
		const action = (window as unknown as { streamingTest: { update(state: unknown): void } }).streamingTest;
		action.update({
			html: '<p><strong>Hello world</strong> <a href="https://example.com">link</a></p>',
			streaming: true,
		});
	});
	await expect(page.locator("#streaming-test strong")).toHaveText("Hello world");
	await expect(page.locator("#streaming-test a")).toHaveAttribute("href", "https://example.com");
	await expect(page.locator("#streaming-test a .message-append")).toHaveText("link");
	await page.evaluate(() => {
		const action = (window as unknown as { streamingTest: { update(state: unknown): void } }).streamingTest;
		action.update({
			html: '<p><strong>Hello world</strong> <a href="https://example.com">link</a></p>',
			streaming: false,
		});
	});
	await expect(page.locator("#streaming-test a .message-append")).toHaveText("link");
});

test("replaced text does not reuse an unrelated animation", async ({ page }) => {
	await page.goto("/");
	const result = await page.evaluate(async () => {
		const path = "/src/lib/streaming-markdown.ts";
		const { streamingMarkdown } = await import(/* @vite-ignore */ path);
		const node = document.createElement("div");
		document.body.append(node);
		const action = streamingMarkdown(node, { html: "<p>Old answer</p>", streaming: true });
		action.update({ html: "<pre><code>New answer</code></pre>", streaming: true });
		return {
			text: node.textContent,
			animations: node.querySelectorAll(".message-append").length,
			html: node.innerHTML,
		};
	});
	expect(result.text).toBe("New answer");
	expect(result.animations).toBe(1);
	expect(result.html).toContain("<pre><code>");
});
