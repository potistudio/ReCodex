import { describe, expect, it } from "vitest";
import { formatTokenCount } from "./usage";

describe("usage display helpers", () => {
	it("formats token counts for compact labels", () => {
		expect(formatTokenCount(12_345)).toMatch(/12[,.]345/);
	});
});
