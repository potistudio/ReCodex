import { describe, expect, it } from "vitest";
import { updateItems } from "./conversation";
import type { Item } from "./types";

describe("app-server item stream", () => {
	it("reconciles optimistic messages and streamed text with completed items", () => {
		let items: Item[] = [
			{
				id: "pending-1",
				type: "userMessage",
				content: [{ type: "text", text: "Hello" }],
			},
		];
		items = updateItems(items, {
			method: "item/started",
			params: {
				item: {
					id: "user-1",
					type: "userMessage",
					content: [{ type: "text", text: "Hello" }],
				},
			},
		});
		for (const delta of ["Hi", " there"])
			items = updateItems(items, {
				method: "item/agentMessage/delta",
				params: { itemId: "assistant-1", delta },
			});
		expect(items).toHaveLength(2);
		expect(items[1].text).toBe("Hi there");
		items = updateItems(items, {
			method: "item/completed",
			params: {
				item: {
					id: "assistant-1",
					type: "agentMessage",
					text: "Hi there!",
				},
			},
		});
		expect(items).toHaveLength(2);
		expect(items[1].text).toBe("Hi there!");
		expect(items[0].id).toBe("user-1");
	});
	it("keeps interleaved command output associated with its item", () => {
		const items: Item[] = [
			{ id: "a", type: "commandExecution" },
			{ id: "b", type: "commandExecution" },
		];
		const result = updateItems(items, {
			method: "item/commandExecution/outputDelta",
			params: { itemId: "a", delta: "output" },
		});
		expect(result[0].aggregatedOutput).toBe("output");
		expect(result[1].aggregatedOutput).toBeUndefined();
		expect(items[0].aggregatedOutput).toBeUndefined();
	});
	it("keeps streamed command output when its lifecycle item is refreshed", () => {
		let items: Item[] = [
			{
				id: "command-1",
				type: "commandExecution",
				command: "pnpm check",
				status: "inProgress",
			},
		];
		items = updateItems(items, {
			method: "item/commandExecution/outputDelta",
			params: { itemId: "command-1", delta: "Checking…\n" },
		});
		items = updateItems(items, {
			method: "item/completed",
			params: {
				item: {
					id: "command-1",
					type: "commandExecution",
					command: "pnpm check",
					status: "completed",
				},
			},
		});
		expect(items[0]).toMatchObject({ status: "completed", aggregatedOutput: "Checking…\n" });
	});
	it("shows output that arrives before the command item", () => {
		const items = updateItems([], {
			method: "item/commandExecution/outputDelta",
			params: { itemId: "command-1", delta: "Checking…\n" },
		});
		expect(items[0]).toMatchObject({
			id: "command-1",
			type: "commandExecution",
			status: "inProgress",
			aggregatedOutput: "Checking…\n",
		});
	});
	it("keeps a streamed assistant message associated with its turn", () => {
		let items = updateItems([], {
			method: "item/agentMessage/delta",
			params: { turnId: "turn-1", itemId: "assistant-1", delta: "Hello" },
		});
		items = updateItems(items, {
			method: "item/completed",
			params: {
				turnId: "turn-1",
				item: { id: "assistant-1", type: "agentMessage", text: "Hello" },
			},
		});
		expect(items[0]).toMatchObject({ turnId: "turn-1", text: "Hello" });
	});
});
