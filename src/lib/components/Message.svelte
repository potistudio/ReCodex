<script lang="ts">
import { Check, ChevronRight, Copy, FileDiff, Sparkles, Terminal } from "@lucide/svelte";
import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { Button } from "$lib/components/ui/button";
import type { Item, TokenUsageBreakdown } from "$lib/types";
import { formatTokenCount } from "$lib/usage";

let { item, tokenUsage = null }: { item: Item; tokenUsage?: TokenUsageBreakdown | null } = $props();
let copied = $state(false);
let copyError = $state("");
let commandExpanded = $state(false);
let commandOutput = $state<HTMLPreElement>();
const text = $derived(
	item.type === "userMessage"
		? (item.content?.map((part) => (typeof part === "string" ? "" : (part.text ?? ""))).join("\n") ?? "")
		: (item.text ?? ""),
);
const html = $derived(
	DOMPurify.sanitize(marked.parse(text, { async: false }) as string, {
		FORBID_TAGS: ["img", "style"],
		FORBID_ATTR: ["style"],
	}),
);
async function copy() {
	try {
		await navigator.clipboard.writeText(text);
		copied = true;
		setTimeout(() => (copied = false), 1800);
	} catch {
		copyError = "Could not copy to clipboard";
	}
}
function link(event: MouseEvent) {
	const anchor = (event.target as HTMLElement).closest("a");
	if (!anchor) return;
	event.preventDefault();
	if (/^https?:\/\//.test(anchor.href)) {
		if (isTauri()) void openUrl(anchor.href).catch(() => {});
		else window.open(anchor.href, "_blank", "noopener,noreferrer");
	}
}
function commandIsRunning(status?: string) {
	return status === "inProgress" || status === "in_progress";
}
function commandStatus(status?: string) {
	if (commandIsRunning(status)) return "Running";
	if (status === "completed") return "Completed";
	if (status === "failed") return "Failed";
	if (status === "cancelled") return "Cancelled";
	return status || "Starting";
}
$effect(() => {
	if (commandIsRunning(item.status)) commandExpanded = true;
});
$effect(() => {
	item.aggregatedOutput;
	if (commandOutput) commandOutput.scrollTop = commandOutput.scrollHeight;
});
</script>

{#if item.type === "userMessage"}
	<div class="user-message"><div class:message-sent={item.renderKey}>{text}</div></div>
{:else if item.type === "agentMessage" || item.type === "plan"}
	<article class="assistant-message">
		<div class="message-author">
			<span class="mini-mark">⌘</span>
			ReCodex
			{#if item.type === "agentMessage" && tokenUsage}
				<span class="message-token-usage">· {formatTokenCount(tokenUsage.totalTokens)} tokens</span>
			{/if}
			{#if item.type === "plan"}
				<span class="muted">· Plan</span>
			{/if}
		</div>
		{#if item.streaming}
			<div class="markdown streaming-markdown">
				{#each item.streamSegments ?? [] as segment, index (index)}
					<span class="message-append">{segment}</span>
				{/each}
			</div>
		{:else}
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<!-- biome-ignore lint/a11y/noStaticElementInteractions: Delegates clicks from rendered markdown links. -->
			<!-- biome-ignore lint/a11y/useKeyWithClickEvents: Keyboard activation is provided by the rendered markdown links. -->
			<div class="markdown" onclick={link}>{@html html}</div>
		{/if}
		{#if text}
			<Button
				variant="ghost"
				size="icon-xs"
				aria-label="Copy message"
				title={copyError || "Copy message"}
				onclick={copy}
				>{#if copied}
					<Check />
				{:else}
					<Copy />
				{/if}</Button
			>
		{/if}
	</article>
{:else if item.type === "commandExecution"}
	<details class="tool-item" class:running={commandIsRunning(item.status)} bind:open={commandExpanded}>
		<summary>
			<Terminal size={15} /><span class="truncate">{item.command || "Command starting…"}</span
			><span class="tool-status">{commandStatus(item.status)}</span>
			<ChevronRight size={13} />
		</summary>
		<pre bind:this={commandOutput} aria-live="polite">{item.aggregatedOutput || "Waiting for output…"}</pre>
	</details>
{:else if item.type === "fileChange"}
	<details class="tool-item">
		<summary>
			<FileDiff size={15} /><span>Changed {item.changes?.length ?? 0} files</span
			><span class="tool-status">{item.status}</span><ChevronRight size={13} />
		</summary>
		{#each item.changes ?? [] as change}
			<div class="diff-name">
				{change.path} <span class="muted">{change.kind.type}</span>
			</div>
			<pre class="diff">{change.diff}</pre>
		{/each}
	</details>
{:else if item.type !== "reasoning"}
	<div class="activity">
		<Sparkles size={14} /><span>{item.tool ?? item.type}</span><span>{item.status ?? ""}</span>
	</div>
{/if}
