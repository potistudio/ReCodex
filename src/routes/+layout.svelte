<script lang="ts">
import "../app.css";

let { children } = $props();
</script>

<div class="app-frame">
	{@render children()}
</div>
