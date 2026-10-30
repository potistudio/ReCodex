// Deterministic app-server fixture: no credentials, network, or model usage.
import { createInterface } from "node:readline";

const root = process.env.RECODEX_TEST_PROJECT;
const privateRoot = process.env.RECODEX_TEST_PRIVATE;
let turnNumber = 0;
const active = new Map();
const approvals = new Map();
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
		const pending = approvals.get(request.id);
		if (pending) {
			approvals.delete(request.id);
			active.delete(pending.threadId);
			send({
				method: "turn/completed",
				params: { threadId: pending.threadId, turn: { ...pending.turn, status: "completed" } },
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
		const activeThread = params.threadId;
		const turn = { id: `turn-${turnNumber}`, status: "inProgress" };
		active.set(activeThread, turn);
		const approvalId = 7 + turnNumber;
		approvals.set(approvalId, { threadId: activeThread, turn });
		send({ method: "turn/started", params: { threadId: activeThread, turn } });
		send({
			id: approvalId,
			method: "item/commandExecution/requestApproval",
			params: { threadId: activeThread, turnId: turn.id, command: "test" },
		});
		result = { turn, received: params };
	} else if (method === "turn/interrupt") {
		const turn = active.get(params.threadId);
		if (turn?.id === params.turnId) {
			active.delete(params.threadId);
			for (const [id, pending] of approvals) if (pending.turn.id === turn.id) approvals.delete(id);
			send({
				method: "turn/completed",
				params: { threadId: params.threadId, turn: { ...turn, status: "interrupted" } },
			});
		}
	}
	send({ id: request.id, result });
}
