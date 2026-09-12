import type { RateLimitWindow } from "./types";

export function formatTokenCount(tokens: number) {
	return new Intl.NumberFormat().format(tokens);
}

export function remainingPercent(usedPercent: number) {
	return Math.max(0, Math.min(100, 100 - usedPercent));
}

export function formatUsageWindow(window: RateLimitWindow) {
	const duration = window.windowDurationMins;
	if (!duration) return "Current limit";
	if (duration % 10080 === 0) return `${duration / 10080} week limit`;
	if (duration % 1440 === 0) return `${duration / 1440} day limit`;
	if (duration % 60 === 0) return `${duration / 60} hour limit`;
	return `${duration} minute limit`;
}

export function formatResetTime(resetsAt: number | null) {
	if (!resetsAt) return null;
	return new Intl.DateTimeFormat(undefined, {
		month: "short",
		day: "numeric",
		hour: "numeric",
		minute: "2-digit",
	}).format(new Date(resetsAt * 1000));
}
