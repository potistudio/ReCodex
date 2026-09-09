import { describe, expect, it } from "vitest";
import { workingState } from "./working";

describe("workingState", () => {
	it("labels a reasoning item as thinking", () => {
		expect(workingState([{ id: "reasoning-1", type: "reasoning" }], false)).toEqual({ label: "Thinking" });
	});

	it("shows Codex's live thinking label", () => {
		expect(workingState([], false, "Assessing the request")).toEqual({ label: "Assessing the request" });
	});

	it("describes an in-progress command", () => {
		expect(
			workingState(
				[{ id: "command-1", type: "commandExecution", command: "pnpm test", status: "inProgress" }],
				false,
			),
		).toEqual({ label: "Running command", detail: "pnpm test" });
	});

	it("gives approvals precedence over streamed activity", () => {
		expect(workingState([{ id: "command-1", type: "commandExecution", status: "inProgress" }], true)).toEqual({
			label: "Waiting for your input",
		});
	});
});
