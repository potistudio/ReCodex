<script lang="ts">
import { ChevronDown, RotateCcw, Zap } from "@lucide/svelte";
import { Popover } from "bits-ui";
import { appear, rollIn, rollOut } from "$lib/motion";
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

const effortOrder = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

let open = $state(false);
let triggerLabelElement = $state<HTMLSpanElement>();
let triggerLabelBounds = $state<DOMRectReadOnly>();
let triggerLabelWidth = $state<number>();
let previousEffortIndex = $state<number>();
let effortRollDirection = $state(1);
let previousModelIndex = $state<number>();
let modelRollDirection = $state(1);

const selectedModel = $derived(models.find((entry) => entry.model === model));
const modelIndex = $derived(
	Math.max(
		0,
		models.findIndex((entry) => entry.model === model),
	),
);
const selectedEffort = $derived(effort || selectedModel?.defaultReasoningEffort || "");
const effortScale = $derived(
	[...new Set(models.flatMap((entry) => entry.supportedReasoningEfforts.map((item) => item.reasoningEffort)))].sort(
		(a, b) => {
			const aIndex = effortOrder.indexOf(a);
			const bIndex = effortOrder.indexOf(b);
			return (aIndex < 0 ? effortOrder.length : aIndex) - (bIndex < 0 ? effortOrder.length : bIndex);
		},
	),
);
const effortIndex = $derived(Math.max(0, effortScale.indexOf(selectedEffort)));
const triggerLabel = $derived(
	open ? "Selecting model..." : `${selectedModel?.displayName || "Codex"} · ${effortLabel(selectedEffort)}`,
);

$effect.pre(() => {
	if (previousEffortIndex !== undefined && effortIndex !== previousEffortIndex) {
		effortRollDirection = Math.sign(effortIndex - previousEffortIndex);
	}
	previousEffortIndex = effortIndex;
	if (previousModelIndex !== undefined && modelIndex !== previousModelIndex) {
		modelRollDirection = Math.sign(previousModelIndex - modelIndex);
	}
	previousModelIndex = modelIndex;
});

$effect(() => {
	if (!open && triggerLabelBounds && triggerLabelElement) {
		triggerLabelWidth = triggerLabelElement.getBoundingClientRect().width;
	}
});

$effect(() => {
	if (disabled) open = false;
});

function effortLabel(value: string) {
	if (value === "xhigh") return "Extra high";
	return value ? value.charAt(0).toUpperCase() + value.slice(1) : "Select effort";
}

function effortDescription(modelOption: Model, effortValue: string) {
	return modelOption.supportedReasoningEfforts.find((item) => item.reasoningEffort === effortValue)?.description;
}

function selectGear(modelOption: Model, effortValue: string) {
	if (disabled || !effortDescription(modelOption, effortValue)) return;
	if (modelOption.model !== model) onModelChange(modelOption.model);
	effort = effortValue;
}
</script>

<Popover.Root bind:open>
	<Popover.Trigger
		class="model-effort-trigger"
		disabled={disabled || !models.length}
		aria-label={triggerLabel}
		title={selectedModel?.displayName}
	>
		<span
			class="model-effort-trigger-copy"
			style:width={triggerLabelWidth === undefined ? undefined : `${triggerLabelWidth}px`}
		>
			<span
				class="model-effort-trigger-label"
				bind:this={triggerLabelElement}
				bind:contentRect={triggerLabelBounds}
			>
				{#key triggerLabel}
					<span class:model-effort-selecting={open} in:appear={{ y: 4 }}>{triggerLabel}</span>
				{/key}
			</span>
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
				<div class="model-effort-current" aria-live="polite">
					<span class="motion-copy effort-label-copy">
						{#key selectedEffort}
							<span
								class="effort-label-value"
								in:rollIn={{ y: 8 * effortRollDirection }}
								out:rollOut={{ y: -8 * effortRollDirection }}
								>{effortLabel(selectedEffort)}</span
							>
						{/key}
					</span>
					<span class="motion-copy model-effort-name-copy">
						{#key model}
							<span
								class="model-effort-name"
								in:rollIn={{ y: 8 * modelRollDirection }}
								out:rollOut={{ y: -8 * modelRollDirection }}
								>{selectedModel?.displayName || "Codex"}</span
							>
						{/key}
					</span>
				</div>
				<button
					type="button"
					class="effort-reset"
					aria-label="Reset reasoning effort"
					title={`Reset to ${effortLabel(selectedModel?.defaultReasoningEffort || "")}`}
					disabled={disabled || !selectedModel?.defaultReasoningEffort}
					onclick={() => selectedModel && selectGear(selectedModel, selectedModel.defaultReasoningEffort)}
				>
					<RotateCcw size={14} aria-hidden="true" />
				</button>
			</div>

			<div class="model-effort-grid" style:--model-columns={models.length}>
				<span class="model-effort-grid-corner" aria-hidden="true"></span>
				{#each models as modelOption (modelOption.model)}
					<span
						class="model-effort-grid-model"
						class:active={modelOption.model === model}
						title={modelOption.displayName}
						>{modelOption.displayName}</span
					>
				{/each}
				{#each effortScale as effortValue (effortValue)}
					<span class="model-effort-grid-effort" class:active={effortValue === selectedEffort}
						>{effortLabel(effortValue)}</span
					>
					{#each models as modelOption (modelOption.model)}
						{#if effortDescription(modelOption, effortValue)}
							<button
								type="button"
								class="model-effort-grid-cell"
								class:active={modelOption.model === model && effortValue === selectedEffort}
								aria-label={`${modelOption.displayName}, ${effortLabel(effortValue)}`}
								aria-pressed={modelOption.model === model && effortValue === selectedEffort}
								title={effortDescription(modelOption, effortValue)}
								{disabled}
								onclick={() => selectGear(modelOption, effortValue)}
							>
								<span aria-hidden="true"></span>
							</button>
						{:else}
							<span class="model-effort-grid-cell unavailable" aria-hidden="true"></span>
						{/if}
					{/each}
				{/each}
			</div>
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
