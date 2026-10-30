import { describe, expect, it } from "vitest";
import {
	estimateTokens,
	type GenerationMeasurement,
	generationRate,
	recentGenerationRate,
	recordText,
	recordUsage,
} from "./generation-speed";
import { formatTokenCount, formatUsageWindow, remainingPercent } from "./usage";

describe("usage display helpers", () => {
	it("formats token counts for compact labels", () => {
		expect(formatTokenCount(12_345)).toMatch(/12[,.]345/);
	});

	it("formats the remaining allowance and common rate-limit windows", () => {
		expect(remainingPercent(61)).toBe(39);
		expect(formatUsageWindow({ usedPercent: 0, windowDurationMins: 300, resetsAt: null })).toBe("5 hour limit");
	});
});

describe("generation speed", () => {
	it("estimates visible text across ASCII and non-ASCII characters", () => {
		expect(estimateTokens("hello 日本😀")).toBe(5);
		expect(generationRate(20, 1000, 3000)).toBe(10);
		expect(generationRate(20, 1000, 1000)).toBeNull();
	});

	it("uses recent output instead of counting the wait before the first text", () => {
		let measurement: GenerationMeasurement = {
			firstTextAt: null,
			lastTextAt: null,
			endedAt: null,
			baseline: null,
			outputTokens: null,
			text: "",
			samples: [],
		};
		measurement = recordText(measurement, "aaaa", 10000);
		expect(recentGenerationRate(measurement.samples)).toBeNull();
		measurement = recordText(measurement, "bbbbbbbb", 11000);
		expect(recentGenerationRate(measurement.samples)).toBe(2);
		measurement = recordText(measurement, "cccc", 12000);
		expect(recentGenerationRate(measurement.samples)).toBe(1.5);
		expect(generationRate(540, measurement.firstTextAt ?? -1, measurement.lastTextAt ?? -1)).toBe(270);
		measurement = recordText(measurement, "dddd", 18000);
		expect(recentGenerationRate(measurement.samples)).toBeNull();
	});

	it("counts every model call in the turn from cumulative usage", () => {
		const breakdown = (outputTokens: number) => ({
			totalTokens: 0,
			inputTokens: 0,
			cachedInputTokens: 0,
			cacheWriteInputTokens: 0,
			outputTokens,
			reasoningOutputTokens: 0,
		});
		const initial = {
			firstTextAt: null,
			lastTextAt: null,
			endedAt: null,
			baseline: null,
			outputTokens: null,
			text: "",
			samples: [],
		};
		const first = recordUsage(initial, { total: breakdown(292), last: breakdown(49), modelContextWindow: null });
		const second = recordUsage(first, { total: breakdown(316), last: breakdown(24), modelContextWindow: null });
		expect(first.outputTokens).toBe(49);
		expect(second.outputTokens).toBe(73);
	});
});
