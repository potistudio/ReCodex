// Svelte handles TypeScript in `<script lang="ts">` natively; the preprocessor only
// covers the remaining `lang` attributes (e.g. `lang="postcss"` in style blocks).
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/vite-plugin-svelte').SvelteConfig} */
const config = {
	preprocess: vitePreprocess(),
};

export default config;
