<script lang="ts">
import "../app.css";
import { onMount } from "svelte";

let { children } = $props();

onMount(() => {
	if (!import.meta.env.DEV) return;

	let stopStudio: (() => void) | undefined;
	void import("cssstudio").then(({ startStudio }) => {
		stopStudio = startStudio();
	});

	return () => stopStudio?.();
});
</script>

{@render children()}
