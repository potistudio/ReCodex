import type { ThreadTokenUsage } from "./types";

export interface GenerationMeasurement {
	firstTextAt: number | null;
	lastTextAt: number | null;
	endedAt: number | null;
	baseline: number | null;
	outputTokens: number | null;
	text: string;
	samples: { at: number; tokens: number }[];
}

// Rough heuristic: four ASCII characters or one non-ASCII character per token.
// It measures visible text only; reasoning and tool calls are not available here.
export function estimateTokens(text: string) {
	let ascii = 0;
	let other = 0;
	for (const char of text) {
		if ((char.codePointAt(0) ?? 0) <= 0x7f) ascii++;
		else other++;
	}
	return Math.ceil(ascii / 4 + other);
}

// Keep one sample just before the window as a baseline for the token delta.
export function recordText(measurement: GenerationMeasurement, delta: string, now: number): GenerationMeasurement {
	const text = measurement.text + delta;
	const previous = measurement.samples;
	const samples = previous.length && now - previous[previous.length - 1].at <= 5000 ? [...previous] : [];
	while (samples.length > 1 && samples[1].at <= now - 5000) samples.shift();
	samples.push({ at: now, tokens: estimateTokens(text) });
	return { ...measurement, text, samples, firstTextAt: measurement.firstTextAt ?? now, lastTextAt: now };
}

export function recentGenerationRate(samples: GenerationMeasurement["samples"]) {
	if (samples.length < 2) return null;
	const first = samples[0];
	const last = samples[samples.length - 1];
	if (last.at - first.at < 250) return null;
	return generationRate(last.tokens - first.tokens, first.at, last.at);
}

export function recordUsage(measurement: GenerationMeasurement, usage: ThreadTokenUsage): GenerationMeasurement {
	const total = usage.total.outputTokens;
	const baseline = measurement.baseline ?? Math.max(0, total - usage.last.outputTokens);
	return { ...measurement, baseline, outputTokens: Math.max(0, total - baseline) };
}

export function generationRate(tokens: number, startedAt: number, endedAt: number) {
	const seconds = (endedAt - startedAt) / 1000;
	return seconds > 0 ? tokens / seconds : null;
}
