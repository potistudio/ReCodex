import { describe, expect, it } from "vitest";
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
