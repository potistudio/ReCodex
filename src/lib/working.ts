import type { Item } from "./types";

export interface WorkingState {
	label: string;
	detail?: string;
}

function isInProgress(status?: string) {
	return status === "inProgress" || status === "in_progress";
}

export function workingState(items: Item[], waitingForInput: boolean, thinkingLabel = ""): WorkingState {
	if (waitingForInput) return { label: "Waiting for your input" };
	if (thinkingLabel) return { label: thinkingLabel };
	const latest = items.at(-1);
	if (!latest) return { label: "Thinking" };
	if (latest.type === "reasoning") return { label: "Thinking" };
	if (latest.type === "plan") return { label: "Planning" };
	if (latest.type === "agentMessage") return { label: "Writing response" };
	if (latest.type === "commandExecution") {
		if (isInProgress(latest.status)) return { label: "Running command", detail: latest.command };
		return { label: "Analyzing command output", detail: latest.command };
	}
	if (latest.type === "fileChange")
		return { label: isInProgress(latest.status) ? "Applying file changes" : "Reviewing file changes" };
	if (latest.type === "mcpToolCall" || latest.type === "dynamicToolCall") {
		const tool = [latest.server, latest.tool].filter(Boolean).join(" / ");
		return {
			label: isInProgress(latest.status) ? "Using tool" : "Analyzing tool result",
			detail: tool || undefined,
		};
	}
	if (latest.type === "contextCompaction") return { label: "Managing context" };
	if (latest.type === "webSearch") return { label: "Searching the web" };
	if (latest.type === "imageView") return { label: "Viewing image" };
	if (latest.type === "imageGeneration") return { label: "Generating image" };
	if (latest.type === "collabAgentToolCall") return { label: "Coordinating agents" };
	if (latest.type === "subAgentActivity") return { label: "Waiting for sub-agent" };
	return { label: "Thinking" };
}
