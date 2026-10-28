<script lang="ts">
import { ChevronDown, ChevronRight, RotateCcw, Zap } from "@lucide/svelte";
import { Popover } from "bits-ui";
import { tick } from "svelte";
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

let open = $state(false);
let choosingModel = $state(false);
let modelList = $state<HTMLFieldSetElement>();
let modelButton = $state<HTMLButtonElement>();
let triggerLabelElement = $state<HTMLSpanElement>();
let triggerLabelBounds = $state<DOMRectReadOnly>();
let triggerLabelWidth = $state<number>();
let pointerProgress = $state<number | null>(null);
let previousEffortIndex = $state<number>();
let effortRollDirection = $state(1);
let previousModelIndex = $state<number>();
let modelRollDirection = $state(1);
let drag: { pointerId: number; slider: HTMLInputElement; grabOffset: number } | null = null;
const selectedModel = $derived(models.find((entry) => entry.model === model));
const modelIndex = $derived(
	Math.max(
		0,
		models.findIndex((entry) => entry.model === model),
	),
);
const efforts = $derived(selectedModel?.supportedReasoningEfforts ?? []);
const selectedEffort = $derived(effort || selectedModel?.defaultReasoningEffort || "");
const triggerLabel = $derived(
	open ? "Selecting model..." : `${selectedModel?.displayName || "Codex"} · ${effortLabel(selectedEffort)}`,
);
const effortLabels = $derived([
	...new Set([...efforts.map((option) => effortLabel(option.reasoningEffort)), effortLabel(selectedEffort)]),
]);
const effortIndex = $derived(
	Math.max(
		0,
		efforts.findIndex((option) => option.reasoningEffort === selectedEffort),
	),
);
const effortProgress = $derived(efforts.length > 1 ? effortIndex / (efforts.length - 1) : 0);

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

function effortLabel(value: string) {
	if (value === "xhigh") return "Extra high";
	return value ? value.charAt(0).toUpperCase() + value.slice(1) : "Select effort";
}

$effect(() => {
	if (!open && triggerLabelBounds && triggerLabelElement) {
		// Measure the rendered label on close; the observer may still contain the placeholder's width.
		triggerLabelWidth = triggerLabelElement.getBoundingClientRect().width;
	}
});

function updatePointerProgress(event: PointerEvent) {
	if (!drag || event.pointerId !== drag.pointerId) return;
	const bounds = drag.slider.getBoundingClientRect();
	const progress = Math.max(
		0,
		Math.min(1, (event.clientX - bounds.left - 14 - drag.grabOffset) / (bounds.width - 28)),
	);
	const intervals = efforts.length - 1;
	const position = progress * intervals;
	const interval = Math.floor(position);
	const t = position - interval;
	// Zero slope at each step creates spatial detents without time-based interpolation.
	const towardNext = t ** 5;
	const towardPrevious = (1 - t) ** 5;
	const eased = towardNext / (towardPrevious + towardNext);
	pointerProgress = (interval + eased) / intervals;
}

function startPointerDrag(event: PointerEvent) {
	if (disabled || efforts.length < 2 || !event.isPrimary || event.button !== 0) return;
	const slider = event.currentTarget as HTMLInputElement;
	const thumb = slider.parentElement?.querySelector(".effort-thumb")?.getBoundingClientRect();
	const grabOffset =
		thumb && event.clientX >= thumb.left && event.clientX <= thumb.right
			? event.clientX - (thumb.left + thumb.width / 2)
			: 0;
	drag = { pointerId: event.pointerId, slider, grabOffset };
	slider.setPointerCapture(event.pointerId);
	updatePointerProgress(event);
}

function endPointerDrag(event?: PointerEvent) {
	if (!drag || (event && event.pointerId !== drag.pointerId)) return;
	const { slider, pointerId } = drag;
	drag = null;
	pointerProgress = null;
	if (slider.hasPointerCapture(pointerId)) slider.releasePointerCapture(pointerId);
}

$effect(() => {
	if (disabled) open = false;
	if (!open) {
		endPointerDrag();
		choosingModel = false;
	}
	return endPointerDrag;
});
</script>

{#snippet effortText()}
	<span class="motion-copy effort-label-copy">
		{#each effortLabels as label (label)}
			<span class="effort-label-size" aria-hidden="true">{label}</span>
		{/each}
		{#key selectedEffort}
			<span
				class="effort-label-value"
				in:rollIn={{ y: 8 * effortRollDirection }}
				out:rollOut={{ y: -8 * effortRollDirection }}
				>{effortLabel(selectedEffort)}</span
			>
		{/key}
	</span>
{/snippet}

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
			onEscapeKeydown={(event) => {
				if (choosingModel) {
					event.preventDefault();
					choosingModel = false;
					void tick().then(() => modelButton?.focus());
				}
			}}
		>
			<div class="model-effort-summary">
				<span class="model-effort-icon" aria-hidden="true"><Zap size={15} fill="currentColor" /></span>
				<button
					type="button"
					class="model-picker-trigger"
					aria-label="Choose model"
					aria-expanded={choosingModel}
					aria-controls="model-picker-list"
					bind:this={modelButton}
					{disabled}
					onclick={() => {
						choosingModel = !choosingModel;
						if (choosingModel) void tick().then(() => modelList?.querySelector<HTMLButtonElement>('[aria-pressed="true"]')?.focus());
					}}
				>
					<span>
						<span class="model-effort-label">
							{@render effortText()}
							<ChevronRight size={12} aria-hidden="true" />
						</span>
						<span class="motion-copy model-effort-name-copy">
							{#key model}
								<span
									class="model-effort-name"
									in:rollIn={{ y: 8 * modelRollDirection }}
									out:rollOut={{ y: -8 * modelRollDirection }}
								>
									{selectedModel?.displayName || "Codex"}
								</span>
							{/key}
						</span>
					</span>
				</button>
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
			<div class="model-picker-views" class:choosing-model={choosingModel}>
				<div class="model-picker-view model-picker-effort" inert={choosingModel}>
					{#if efforts.length}
						<div
							class="effort-control"
							class:dragging={pointerProgress !== null}
							style:--effort-progress={pointerProgress ?? effortProgress}
						>
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
								onpointerdown={startPointerDrag}
								onpointermove={updatePointerProgress}
								onpointerup={endPointerDrag}
								onpointercancel={endPointerDrag}
								onlostpointercapture={endPointerDrag}
								oninput={(event) => {
									const option = efforts[event.currentTarget.valueAsNumber];
									if (option) effort = option.reasoningEffort;
								}}
							>
						</div>
					{/if}
				</div>
				<div class="model-picker-view model-picker-models" inert={!choosingModel}>
					<fieldset
						class="model-picker-list"
						id="model-picker-list"
						bind:this={modelList}
						aria-label="Models"
					>
						{#each models as option (option.model)}
							<button
								type="button"
								class="model-option"
								aria-pressed={option.model === model}
								{disabled}
								onclick={() => {
									onModelChange(option.model);
									choosingModel = false;
									void tick().then(() => modelButton?.focus());
								}}
							>
								<span><strong>{option.displayName}</strong><small>{option.description}</small></span>
								{#if option.model === model}
									<span aria-hidden="true">✓</span>
								{/if}
							</button>
						{/each}
					</fieldset>
				</div>
			</div>
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
