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
let dragging = $state(false);
let pointerId: number | null = null;

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
const modelProgress = $derived(models.length > 1 ? modelIndex / (models.length - 1) : 0.5);
const effortProgress = $derived(effortScale.length > 1 ? effortIndex / (effortScale.length - 1) : 0.5);
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
	if (!open) endDrag();
	return endDrag;
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

function nearestSupportedEffort(modelOption: Model, targetIndex: number) {
	return modelOption.supportedReasoningEfforts.reduce((nearest, option) => {
		const distance = Math.abs(effortScale.indexOf(option.reasoningEffort) - targetIndex);
		const nearestDistance = Math.abs(effortScale.indexOf(nearest.reasoningEffort) - targetIndex);
		return distance < nearestDistance ? option : nearest;
	}).reasoningEffort;
}

function selectFromPointer(event: PointerEvent, gate: HTMLElement) {
	const bounds = gate.getBoundingClientRect();
	const x = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
	const y = Math.max(0, Math.min(1, (event.clientY - bounds.top) / bounds.height));
	const targetModelIndex = Math.round(x * Math.max(0, models.length - 1));
	const targetEffortIndex = Math.round(y * Math.max(0, effortScale.length - 1));
	const targetModel = models[targetModelIndex];
	if (!targetModel?.supportedReasoningEfforts.length) return;
	selectGear(targetModel, nearestSupportedEffort(targetModel, targetEffortIndex));
}

function startDrag(event: PointerEvent) {
	if (disabled || !event.isPrimary || event.button !== 0) return;
	const surface = event.currentTarget as HTMLElement;
	const gate = surface.parentElement;
	if (!gate) return;
	pointerId = event.pointerId;
	dragging = true;
	surface.setPointerCapture(event.pointerId);
	selectFromPointer(event, gate);
}

function moveDrag(event: PointerEvent) {
	if (!dragging || event.pointerId !== pointerId) return;
	const gate = (event.currentTarget as HTMLElement).parentElement;
	if (gate) selectFromPointer(event, gate);
}

function endDrag(event?: PointerEvent) {
	if (!dragging || (event && event.pointerId !== pointerId)) return;
	const gate = event?.currentTarget as HTMLElement | undefined;
	if (gate && pointerId !== null && gate.hasPointerCapture(pointerId)) gate.releasePointerCapture(pointerId);
	dragging = false;
	pointerId = null;
}

function moveModel(offset: number) {
	const target = models[Math.max(0, Math.min(models.length - 1, modelIndex + offset))];
	if (!target?.supportedReasoningEfforts.length) return;
	const targetEffort = target.supportedReasoningEfforts.some((item) => item.reasoningEffort === selectedEffort)
		? selectedEffort
		: target.defaultReasoningEffort;
	selectGear(target, targetEffort);
}

function moveEffort(offset: number) {
	if (!selectedModel) return;
	const supported = selectedModel.supportedReasoningEfforts;
	const index = Math.max(
		0,
		supported.findIndex((item) => item.reasoningEffort === selectedEffort),
	);
	const target = supported[Math.max(0, Math.min(supported.length - 1, index + offset))];
	if (target) selectGear(selectedModel, target.reasoningEffort);
}

function handleKeydown(event: KeyboardEvent) {
	if (event.key === "ArrowLeft") moveModel(-1);
	else if (event.key === "ArrowRight") moveModel(1);
	else if (event.key === "ArrowUp") moveEffort(-1);
	else if (event.key === "ArrowDown") moveEffort(1);
	else return;
	event.preventDefault();
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

			<div
				class="gear-selector"
				style:--gear-columns={models.length}
				style:--gear-rows={effortScale.length}
				style:--gear-x={modelProgress}
				style:--gear-y={effortProgress}
			>
				<div class="gear-model-labels" aria-hidden="true">
					{#each models as option (option.model)}
						<span class:active={option.model === model} title={option.displayName}
							>{option.displayName}</span
						>
					{/each}
				</div>
				<div class="gear-effort-labels" aria-hidden="true">
					{#each effortScale as effortValue (effortValue)}
						<span class:active={effortValue === selectedEffort}>{effortLabel(effortValue)}</span>
					{/each}
				</div>
				<div class="gear-gate" class:dragging>
					<svg class="gear-gate-lines" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
						<line x1="0" y1="50" x2="100" y2="50" />
						{#each models as _, index}
							<line
								x1={models.length > 1 ? (index / (models.length - 1)) * 100 : 50}
								y1="0"
								x2={models.length > 1 ? (index / (models.length - 1)) * 100 : 50}
								y2="100"
							/>
						{/each}
					</svg>
					<button
						type="button"
						class="gear-drag-surface"
						aria-label={`Gear selector, ${selectedModel?.displayName || "Codex"}, ${effortLabel(selectedEffort)}`}
						{disabled}
						onkeydown={handleKeydown}
						onpointerdown={startDrag}
						onpointermove={moveDrag}
						onpointerup={endDrag}
						onpointercancel={endDrag}
						onlostpointercapture={endDrag}
					></button>
					{#each models as modelOption, column (modelOption.model)}
						{#each effortScale as effortValue, row (effortValue)}
							{#if effortDescription(modelOption, effortValue)}
								<button
									type="button"
									class="gear-position"
									class:active={modelOption.model === model && effortValue === selectedEffort}
									style:left={`${models.length > 1 ? (column / (models.length - 1)) * 100 : 50}%`}
									style:top={`${effortScale.length > 1 ? (row / (effortScale.length - 1)) * 100 : 50}%`}
									aria-label={`${modelOption.displayName}, ${effortLabel(effortValue)}`}
									title={effortDescription(modelOption, effortValue)}
									{disabled}
									onclick={(event) => {
										if (event.detail === 0) selectGear(modelOption, effortValue);
									}}
									onpointerdown={startDrag}
									onpointermove={moveDrag}
									onpointerup={endDrag}
									onpointercancel={endDrag}
									onlostpointercapture={endDrag}
								></button>
							{/if}
						{/each}
					{/each}
					<span class="gear-lever" aria-hidden="true">
						<span class="gear-knob"></span>
					</span>
				</div>
			</div>
			<p class="gear-hint">Drag the lever · Arrow keys to shift</p>
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
