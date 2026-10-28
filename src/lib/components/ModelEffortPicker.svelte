<script lang="ts">
import { ChevronDown } from "@lucide/svelte";
import * as Dropdown from "$lib/components/ui/dropdown-menu";
import type { Model } from "$lib/types";

let {
	models,
	model,
	effort = $bindable(),
	disabled = false,
	onModelChange,
}: {
	models: Model[];
	model: string;
	effort: string;
	disabled?: boolean;
	onModelChange: (model: string) => void;
} = $props();

const selectedModel = $derived(models.find((entry) => entry.model === model));
const selectedEffort = $derived(effort || selectedModel?.defaultReasoningEffort || "");

function effortLabel(value: string) {
	if (value === "xhigh") return "Extra high";
	return value ? value.charAt(0).toUpperCase() + value.slice(1) : "Select effort";
}
</script>

<Dropdown.Root>
	<Dropdown.Trigger
		class="model-effort-trigger"
		disabled={disabled || !models.length}
		aria-label={`${selectedModel?.displayName || "Codex"} · ${effortLabel(selectedEffort)}`}
	>
		<span>{selectedModel?.displayName || "Codex"} · {effortLabel(selectedEffort)}</span>
		<ChevronDown size={12} aria-hidden="true" />
	</Dropdown.Trigger>
	<Dropdown.Content align="start" side="top" class="w-80 max-h-[min(70vh,32rem)]">
		<Dropdown.Label>Model</Dropdown.Label>
		<Dropdown.RadioGroup value={model} onValueChange={onModelChange} aria-label="Model">
			{#each models as option (option.model)}
				<Dropdown.RadioItem value={option.model} {disabled}>
					<span class="grid gap-0.5">
						<strong class="font-medium">{option.displayName}</strong>
						{#if option.description}
							<small class="text-xs text-muted-foreground">{option.description}</small>
						{/if}
					</span>
				</Dropdown.RadioItem>
			{/each}
		</Dropdown.RadioGroup>
		{#if selectedModel?.supportedReasoningEfforts.length}
			<Dropdown.Separator />
			<Dropdown.Label>Reasoning effort</Dropdown.Label>
			<Dropdown.RadioGroup
				value={selectedEffort}
				onValueChange={(value) => (effort = value)}
				aria-label="Reasoning effort"
			>
				{#each selectedModel.supportedReasoningEfforts as option (option.reasoningEffort)}
					<Dropdown.RadioItem value={option.reasoningEffort} {disabled} title={option.description}>
						{effortLabel(option.reasoningEffort)}
					</Dropdown.RadioItem>
				{/each}
			</Dropdown.RadioGroup>
		{/if}
	</Dropdown.Content>
</Dropdown.Root>
