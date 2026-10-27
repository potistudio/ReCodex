<script lang="ts">
import { Dialog as DialogPrimitive } from "bits-ui";
import { cn } from "$lib/utils.js";

let { ref = $bindable(null), class: className, ...restProps }: DialogPrimitive.OverlayProps = $props();
</script>

<DialogPrimitive.Overlay
	bind:ref
	data-slot="dialog-overlay"
	class={cn(
		"bg-black/30 supports-backdrop-filter:backdrop-blur-sm fixed inset-0 isolate z-50",
		className,
	)}
	{...restProps}
/>
