<script lang="ts">
import "../app.css";
import { registerHyperellipse } from "hyperellipse";
import { onMount } from "svelte";

let { children } = $props();

onMount(() => {
	const controller = registerHyperellipse();
	return () => controller.destroy();
});
</script>

<div class="app-frame">
	{@render children()}
</div>
