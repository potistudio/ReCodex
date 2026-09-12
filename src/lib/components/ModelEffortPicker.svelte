<script lang="ts">
import { ChevronDown, ChevronRight, RotateCcw, Zap } from "@lucide/svelte";
import { Popover } from "bits-ui";
import * as Dropdown from "$lib/components/ui/dropdown-menu";
import { appear } from "$lib/motion";
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

let open = $state(false);
const selectedModel = $derived(models.find((entry) => entry.model === model));
const efforts = $derived(selectedModel?.supportedReasoningEfforts ?? []);
const selectedEffort = $derived(effort || selectedModel?.defaultReasoningEffort || "");
const effortIndex = $derived(
	Math.max(
		0,
		efforts.findIndex((option) => option.reasoningEffort === selectedEffort),
	),
);
const effortProgress = $derived(efforts.length > 1 ? effortIndex / (efforts.length - 1) : 0);

function effortLabel(value: string) {
	if (value === "xhigh") return "Extra high";
	return value ? value.charAt(0).toUpperCase() + value.slice(1) : "Select effort";
}

$effect(() => {
	if (disabled) open = false;
});
</script>

<Popover.Root bind:open>
	<Popover.Trigger
		class="model-effort-trigger"
		disabled={disabled || !models.length}
		title={selectedModel?.displayName}
	>
		<span class="motion-copy">
			{#key `${model}:${selectedEffort}`}
				<span transition:appear={{ y: 4 }}>
					{selectedModel?.displayName || "Codex"}
					· {effortLabel(selectedEffort)}
				</span>
			{/key}
		</span>
		<ChevronDown size={12} aria-hidden="true" />
	</Popover.Trigger>
	<Popover.Portal>
		<Popover.Content
			class="model-effort-popover"
			role="dialog"
			aria-label="Model and reasoning effort"
			side="top"
			align="center"
			sideOffset={8}
			collisionPadding={12}
		>
			<div class="model-effort-summary">
				<span class="model-effort-icon" aria-hidden="true"><Zap size={15} fill="currentColor" /></span>
				<Dropdown.Root>
					<Dropdown.Trigger class="model-picker-trigger" aria-label="Choose model" {disabled}>
						<span class="motion-copy">
							{#key `${model}:${selectedEffort}`}
								<span transition:appear={{ y: 4 }}>
									<span class="model-effort-label"
										>{effortLabel(selectedEffort)}
										<ChevronRight size={12} aria-hidden="true" /></span
									>
									<span class="model-effort-name">{selectedModel?.displayName || "Codex"}</span>
								</span>
							{/key}
						</span>
					</Dropdown.Trigger>
					<Dropdown.Content class="model-picker-menu" side="right" align="start" sideOffset={12}>
						<Dropdown.Label>Model</Dropdown.Label>
						<Dropdown.RadioGroup value={model} onValueChange={onModelChange}>
							{#each models as option (option.model)}
								<Dropdown.RadioItem value={option.model} class="model-option" {disabled}>
									<div><strong>{option.displayName}</strong><small>{option.description}</small></div>
								</Dropdown.RadioItem>
							{/each}
						</Dropdown.RadioGroup>
					</Dropdown.Content>
				</Dropdown.Root>
				<button
					type="button"
					class="effort-reset"
					aria-label="Reset reasoning effort"
					title={`Reset to ${effortLabel(selectedModel?.defaultReasoningEffort || "")}`}
					disabled={disabled || !selectedModel?.defaultReasoningEffort}
					onclick={() => (effort = selectedModel?.defaultReasoningEffort || "")}
				>
					<RotateCcw size={14} aria-hidden="true" />
				</button>
			</div>
			{#if efforts.length}
				<div class="effort-control" style:--effort-progress={effortProgress}>
					<div class="effort-track" aria-hidden="true">
						<div class="effort-fill"></div>
						<div class="effort-ticks">
							{#each efforts as option, index (option.reasoningEffort)}
								<span
									class:filled={index <= effortIndex}
									style:left={`${efforts.length > 1 ? (index / (efforts.length - 1)) * 100 : 0}%`}
								></span>
							{/each}
						</div>
					</div>
					<span class="effort-thumb" aria-hidden="true"></span>
					<input
						type="range"
						class="effort-slider"
						aria-label="Reasoning effort"
						aria-valuetext={effortLabel(selectedEffort)}
						title={efforts[effortIndex]?.description}
						min="0"
						max={Math.max(1, efforts.length - 1)}
						step="1"
						value={effortIndex}
						disabled={disabled || efforts.length < 2}
						oninput={(event) => {
							const option = efforts[event.currentTarget.valueAsNumber];
							if (option) effort = option.reasoningEffort;
						}}
					>
				</div>
			{/if}
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
