<script lang="ts">
import "./app.css";
import { registerHyperellipse } from "hyperellipse";
import { onMount } from "svelte";
import Workspace from "$lib/Workspace.svelte";

onMount(() => {
	const controller = registerHyperellipse();
	return () => controller.destroy();
});
</script>

<div class="app-frame">
	<Workspace />
</div>

<style>
.app-frame {
	display: flex;
	flex-direction: column;
	height: 100dvh;
}
</style>
