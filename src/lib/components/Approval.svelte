<script lang="ts">
import { MessageCircleQuestion, ShieldCheck } from "@lucide/svelte";
import { Button } from "$lib/components/ui/button";
import { Input } from "$lib/components/ui/input";
import type { ServerEvent } from "$lib/types";

let {
	event,
	respond,
	requestAlternative,
}: {
	event: ServerEvent;
	respond: (event: ServerEvent, result: unknown) => Promise<void>;
	requestAlternative: (event: ServerEvent) => Promise<void>;
} = $props();
let answers = $state<Record<string, string>>({});
let submitting = $state(false);
const questions = $derived(event.params.questions ?? []);
const userInput = $derived(event.method === "item/tool/requestUserInput");
const permissions = $derived(event.method === "item/permissions/requestApproval");
const allowed = $derived(event.params.availableDecisions);
const canAccept = $derived(!allowed || allowed.includes("accept") || permissions);
const canAcceptForSession = $derived(allowed?.includes("acceptForSession"));
async function submit(result: unknown) {
	submitting = true;
	await respond(event, result);
	submitting = false;
}

async function declineAndRequestAlternative() {
	submitting = true;
	await requestAlternative(event);
	submitting = false;
}
</script>

<section class="approval-card" aria-label={userInput ? "Input requested" : "Approval requested"}>
	<h3>
		{#if userInput}
			<MessageCircleQuestion size={18} />Your input is needed
		{:else}
			<ShieldCheck size={18} />Permission requested
		{/if}
	</h3>
	{#if event.params.reason}
		<p>{event.params.reason}</p>
	{/if}
	{#if event.params.networkApprovalContext}
		<p>
			Network access:
			{event.params.networkApprovalContext
				.protocol}://{event.params.networkApprovalContext.host}
		</p>
	{/if}
	{#if event.params.command}
		<pre>{event.params.command}</pre>
	{/if}
	{#if event.params.cwd}
		<p class="muted">{event.params.cwd}</p>
	{/if}
	{#if event.params.grantRoot}
		<p>Write access: {event.params.grantRoot}</p>
	{/if}
	{#if permissions}
		<pre>{JSON.stringify(
				event.params.permissions,
				null,
				2,
			)}</pre>
	{/if}
	{#if userInput}
		{#each questions as question}
			<fieldset>
				<legend>{question.question}</legend>
				{#each question.options ?? [] as option}
					<label class="question-option"
						><input type="radio" name={question.id} value={option.label} bind:group={answers[question.id]}>
						<span>{option.label}<small>{option.description}</small></span></label
					>
				{/each}
				<Input
					aria-label={question.question}
					type={question.isSecret ? "password" : "text"}
					placeholder="Your answer"
					bind:value={answers[question.id]}
				/>
			</fieldset>
		{/each}
		<Button
			disabled={submitting ||
				questions.some((question) => !answers[question.id]?.trim())}
			onclick={() =>
				submit({
					answers: Object.fromEntries(
						questions.map((question) => [
							question.id,
							{ answers: [answers[question.id]] },
						]),
					),
				})}
			>Submit answer</Button
		>
	{:else}
		<div class="approval-actions">
			<Button
				variant="outline"
				disabled={submitting}
				onclick={() =>
					submit(permissions ? { permissions: {}, scope: "turn" } : { decision: "decline" })}
				>Decline</Button
			>
			{#if !permissions}
				<Button variant="outline" disabled={submitting} onclick={declineAndRequestAlternative}
					>Ask for another approach</Button
				>
			{/if}
			{#if canAccept}
				<Button
					disabled={submitting}
					onclick={() =>
						submit(
							permissions
								? {
										permissions: event.params.permissions,
										scope: "turn",
									}
								: { decision: "accept" },
						)}
					>Allow once</Button
				>
			{/if}
			{#if canAcceptForSession}
				<Button disabled={submitting} onclick={() => submit({ decision: "acceptForSession" })}
					>Allow for session</Button
				>
			{/if}
		</div>
	{/if}
</section>
