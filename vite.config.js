/// <reference types="vitest/config" />
import process from "node:process";
import { fileURLToPath } from "node:url";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
	// The web client lives in `web/`; Tauri keeps its config in `desktop/`.
	root: fileURLToPath(new URL("./web", import.meta.url)),
	plugins: [tailwindcss(), svelte({ configFile: fileURLToPath(new URL("./svelte.config.js", import.meta.url)) })],

	resolve: {
		alias: {
			$lib: fileURLToPath(new URL("./web/src/lib", import.meta.url)),
		},
	},

	// `web/static/` assets are copied verbatim into the build output, as Tauri expects.
	publicDir: "static",

	build: {
		// `desktop/tauri.conf.json` points `frontendDist` at `../web/dist`.
		outDir: "dist",
		emptyOutDir: true,
	},

	test: {
		// Unit tests are `*.test.ts` next to the code. Playwright specs are `*.spec.ts`, so keep them out.
		include: ["src/**/*.test.ts"],
	},

	// Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
	//
	// 1. prevent Vite from obscuring rust errors
	clearScreen: false,
	// 2. tauri expects a fixed port, fail if that port is not available
	server: {
		port: 1420,
		strictPort: true,
		host: host || "127.0.0.1",
		hmr: host
			? {
					protocol: "ws",
					host,
					port: 1421,
				}
			: undefined,
		watch: {
			// 3. tell Vite to ignore watching `desktop`
			ignored: ["**/desktop/**"],
		},
	},
}));
