import { describe, expect, it } from "vitest";
import { toMilliseconds } from "./motion";

describe("toMilliseconds", () => {
	it("reads durations in milliseconds", () => {
		expect(toMilliseconds("320ms")).toBe(320);
		expect(toMilliseconds(" 160ms ")).toBe(160);
	});

	it("reads durations in seconds", () => {
		// Production builds minify `320ms` to `.32s`.
		expect(toMilliseconds(".32s")).toBe(320);
		expect(toMilliseconds("0.24s")).toBe(240);
	});

	it("falls back to zero for missing or malformed values", () => {
		expect(toMilliseconds("")).toBe(0);
		expect(toMilliseconds("auto")).toBe(0);
	});
});
