import { cubicOut } from "svelte/easing";
import { type FlyParams, fly, type SlideParams, slide } from "svelte/transition";

function duration(node: Element, token: string) {
	return matchMedia("(prefers-reduced-motion: reduce)").matches
		? 0
		: Number.parseFloat(getComputedStyle(node).getPropertyValue(token));
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

function roll(node: Element, y: number) {
	return {
		duration: duration(node, "--motion-fast") * 0.75,
		easing: cubicOut,
		css: (_t: number, u: number) => `transform: translate3d(0, ${y * u}px, 0);`,
	};
}

export function rollIn(node: Element, { y = 20 }: { y?: number } = {}) {
	return roll(node, y);
}
