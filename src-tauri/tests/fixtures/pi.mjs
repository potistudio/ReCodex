import { createInterface } from "node:readline";

const models = [
	{
		id: "claude-sonnet-4",
		name: "Claude Sonnet 4",
		api: "anthropic-messages",
		provider: "anthropic",
		reasoning: true,
		thinkingLevelMap: { xhigh: null },
		contextWindow: 200000,
		maxTokens: 16000,
		usage: { input: 100, output: 20, cacheRead: 0, cacheWrite: 0, reasoning: 5, totalTokens: 120 },
	},
	{
		id: "gpt-5",
		name: "GPT-5",
		api: "openai-responses",
		provider: "openai",
		reasoning: true,
		contextWindow: 400000,
		maxTokens: 32000,
		usage: { input: 200, output: 30, cacheRead: 50, cacheWrite: 0, reasoning: 10, totalTokens: 280 },
	},
];

let current = models[0];
let thinkingLevel = "medium";
let sessionName = "Pi fixture";
let aborted = false;
const history = [];
let turn = 0;

function send(value) {
	process.stdout.write(`${JSON.stringify(value)}\n`);
}

function respond(id, command, data) {
	send({ id, type: "response", command, success: true, ...(data === undefined ? {} : { data }) });
}

function usage() {
	return {
		input: current.usage.input,
		output: current.usage.output,
		cacheRead: current.usage.cacheRead,
		cacheWrite: current.usage.cacheWrite,
		reasoning: current.usage.reasoning,
		totalTokens: current.usage.totalTokens,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	};
}

function message(role, content) {
	return { role, content, timestamp: Date.now() };
}

function emitRun(prompt, id) {
	turn += 1;
	const assistant = {
		role: "assistant",
		content: [
			{ type: "text", text: `Hello from Pi: ${prompt}` },
			{
				type: "toolCall",
				id: `call-${turn}`,
				name: "bash",
				arguments: { command: "echo pi" },
			},
		],
		api: current.api,
		provider: current.provider,
		model: current.id,
		usage: usage(),
		stopReason: "toolUse",
		timestamp: Date.now(),
	};
	const toolResult = {
		role: "toolResult",
		toolCallId: `call-${turn}`,
		toolName: "bash",
		content: [{ type: "text", text: "pi\n" }],
		isError: false,
		timestamp: Date.now(),
	};
	const steps = [
		{ type: "agent_start" },
		{ type: "turn_start" },
		{ type: "message_start", message: message("user", prompt) },
		{ type: "message_end", message: message("user", prompt) },
		{
			type: "message_start",
			message: { ...assistant, content: [], stopReason: "pending" },
		},
		{
			type: "message_update",
			usage: usage(),
			assistantMessageEvent: {
				type: "text_delta",
				contentIndex: 0,
				delta: "Hello ",
			},
		},
		{
			type: "message_update",
			usage: usage(),
			assistantMessageEvent: {
				type: "text_delta",
				contentIndex: 0,
				delta: "from Pi.",
			},
		},
		{ type: "message_end", message: assistant },
		{
			type: "tool_execution_start",
			toolCallId: `call-${turn}`,
			toolName: "bash",
			args: { command: "echo pi" },
		},
		{
			type: "tool_execution_update",
			toolCallId: `call-${turn}`,
			toolName: "bash",
			args: { command: "echo pi" },
			partialResult: { content: [{ type: "text", text: "pi" }] },
		},
		{
			type: "tool_execution_end",
			toolCallId: `call-${turn}`,
			toolName: "bash",
			result: { content: [{ type: "text", text: "pi\n" }] },
			isError: false,
			durationMs: 1,
		},
		{ type: "turn_end", message: assistant, toolResults: [toolResult] },
		{ type: "agent_end", messages: [assistant, toolResult], willRetry: false },
		{ type: "agent_settled", aborted: false },
	];
	history.push(message("user", prompt), assistant, toolResult);
	let index = 0;
	const timer = setInterval(() => {
		if (aborted) {
			clearInterval(timer);
			aborted = false;
			send({ type: "agent_settled", aborted: true });
			return;
		}
		if (index >= steps.length) {
			clearInterval(timer);
			return;
		}
		send(steps[index++]);
	}, 2);
	void id;
}

process.stdout.write(
	`${JSON.stringify({
		type: "extension_ui_request",
		id: "status-1",
		method: "setStatus",
		statusKey: "fixture",
		statusText: "ready",
	})}\n`,
);

const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
	let command;
	try {
		command = JSON.parse(line);
	} catch {
		return;
	}
	const id = command.id;
	switch (command.type) {
		case "get_available_models":
			respond(id, "get_available_models", { models });
			break;
		case "get_state":
			respond(id, "get_state", {
				model: current,
				thinkingLevel,
				isStreaming: false,
				isCompacting: false,
				sessionId: "pi-fixture",
				sessionName,
				messageCount: history.length,
				pendingMessageCount: 0,
			});
			break;
		case "set_model": {
			current =
				models.find((model) => model.provider === command.provider && model.id === command.modelId) ?? current;
			respond(id, "set_model", current);
			break;
		}
		case "set_thinking_level":
			thinkingLevel = command.level ?? thinkingLevel;
			respond(id, "set_thinking_level");
			break;
		case "get_session_stats":
			respond(id, "get_session_stats", {
				tokens: {
					input: current.usage.input,
					output: current.usage.output,
					cacheRead: current.usage.cacheRead,
					cacheWrite: current.usage.cacheWrite,
					total: current.usage.totalTokens,
				},
				cost: 0,
			});
			break;
		case "get_messages":
			respond(id, "get_messages", { messages: history });
			break;
		case "prompt":
			aborted = false;
			respond(id, "prompt", { disposition: "started" });
			emitRun(command.message ?? "", id);
			break;
		case "abort":
			aborted = true;
			respond(id, "abort");
			break;
		case "set_session_name":
			sessionName = command.name;
			respond(id, "set_session_name");
			break;
		default:
			send({
				id,
				type: "response",
				command: command.type,
				success: false,
				error: `Unsupported fixture command: ${command.type}`,
			});
	}
});
lines.on("close", () => process.exit(0));
