import { defineConfig } from "@playwright/test";

export default defineConfig({
	testDir: "./tests",
	fullyParallel: false,
	use: {
		baseURL: "http://127.0.0.1:1420",
		viewport: { width: 1280, height: 860 },
		channel: "msedge",
		screenshot: "only-on-failure",
	},
	webServer: {
		command: "pnpm dev",
		url: "http://127.0.0.1:1420",
		reuseExistingServer: true,
		timeout: 120_000,
	},
});
