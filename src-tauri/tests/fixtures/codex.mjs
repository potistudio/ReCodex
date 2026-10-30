// Deterministic app-server fixture: no credentials, network, or model usage.
import { createInterface } from "node:readline";

const root = process.env.RECODEX_TEST_PROJECT;
const privateRoot = process.env.RECODEX_TEST_PRIVATE;
let turnNumber = 0;
let activeThread = null;
const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const thread = (id = "shared-thread") => ({
	id,
	cwd: id === "private-thread" ? privateRoot : root,
	name: "Test chat",
	preview: "Test",
	updatedAt: 1,
	turns: [],
	model: "test-model",
});

for await (const line of createInterface({ input: process.stdin })) {
	const request = JSON.parse(line);
	const { method, params = {} } = request;
	if (!method) {
		if (request.id === 7 && activeThread) {
			send({
				method: "turn/completed",
				params: { threadId: activeThread, turn: { id: `turn-${turnNumber}`, status: "completed" } },
			});
		}
		continue;
	}
	if (!("id" in request)) continue;
	let result = {};
	if (method === "model/list") {
		result = { data: [{ id: "test-model", model: "test-model" }], nextCursor: null };
	} else if (method === "account/read") {
		result = { account: null, requiresOpenaiAuth: false };
	} else if (method === "thread/list") {
		result = { data: [thread()], nextCursor: null, received: params };
	} else if (["thread/read", "thread/resume", "thread/start"].includes(method)) {
		result = { thread: thread(params.threadId), model: "test-model", reasoningEffort: "medium", received: params };
	} else if (method === "turn/start") {
		turnNumber += 1;
		activeThread = params.threadId;
		const turn = { id: `turn-${turnNumber}`, status: "inProgress" };
		send({ method: "turn/started", params: { threadId: activeThread, turn } });
		send({
			id: 7,
			method: "item/commandExecution/requestApproval",
			params: { threadId: activeThread, turnId: turn.id, command: "test" },
		});
		result = { turn, received: params };
	}
	send({ id: request.id, result });
}
