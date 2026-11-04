import { describe, expect, it } from "vitest";
import { requiresCodexAuth } from "./providers";
import type { Model } from "./types";

function model(provider?: string): Model {
	return {
		id: provider ?? "model",
		model: provider ? `${provider}/model` : "model",
		displayName: "Model",
		description: "",
		isDefault: true,
		hidden: false,
		defaultReasoningEffort: "medium",
		supportedReasoningEfforts: [],
		provider,
	};
}

describe("requiresCodexAuth", () => {
	it("does not block Pi models when Codex needs sign-in", () => {
		expect(requiresCodexAuth(model("pi"), true)).toBe(false);
	});

	it("blocks Codex models when Codex needs sign-in", () => {
		expect(requiresCodexAuth(model("codex"), true)).toBe(true);
	});

	it("does not block Codex models when sign-in is not required", () => {
		expect(requiresCodexAuth(model("codex"), false)).toBe(false);
	});

	it("uses the host requirement before a model is selected", () => {
		expect(requiresCodexAuth(undefined, true)).toBe(true);
	});
});
