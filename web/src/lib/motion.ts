import { cubicOut } from "svelte/easing";
import { type FlyParams, fly, type SlideParams, slide } from "svelte/transition";

// Builds minify `320ms` to `.32s`, so the unit has to be honoured.
export function toMilliseconds(value: string) {
	const match = /^\s*([+-]?(?:\d+\.?\d*|\.\d+))\s*(ms|s)?\s*$/i.exec(value);
	if (!match) return 0;
	const amount = Number.parseFloat(match[1]);
	if (!Number.isFinite(amount)) return 0;
	return match[2]?.toLowerCase() === "s" ? amount * 1000 : amount;
}

function duration(node: Element, token: string) {
	return matchMedia("(prefers-reduced-motion: reduce)").matches
		? 0
		: toMilliseconds(getComputedStyle(node).getPropertyValue(token));
}

export function reveal(node: Element, { axis = "y" }: Pick<SlideParams, "axis"> = {}) {
	const animation = slide(node, { axis, duration: duration(node, "--motion-panel"), easing: cubicOut });
	if (axis === "y") return animation;
	const width = node.clientWidth;
	return {
		...animation,
		// Keep panel contents from reflowing while the outer panel changes width.
		css: (t: number, u: number) => `${animation.css?.(t, u) ?? ""}; --reveal-width: ${width}px`,
	};
}

export function appear(node: Element, { y = 6 }: Pick<FlyParams, "y"> = {}) {
	return fly(node, { y, duration: duration(node, "--motion-normal"), easing: cubicOut });
}

export function rollIn(node: Element, { y = 8 }: Pick<FlyParams, "y"> = {}) {
	return fly(node, { y, duration: duration(node, "--motion-fast") * 0.75, easing: cubicOut });
}

export function rollOut(node: Element, { y = -8 }: Pick<FlyParams, "y"> = {}) {
	return fly(node, { y, duration: duration(node, "--motion-fast") * 0.75, easing: cubicOut });
}
