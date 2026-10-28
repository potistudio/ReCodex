type MarkdownState = { html: string; streaming: boolean | undefined };
type AnimatedRange = { start: number; end: number; started: number };

// Keep in sync with the message-append animation in app.css.
const animationDuration = 320;

// The caller must sanitize the HTML before passing it to this action.
export function streamingMarkdown(node: HTMLElement, initial: MarkdownState) {
	let previousText = "";
	let ranges: AnimatedRange[] = [];

	function update({ html, streaming }: MarkdownState) {
		const now = performance.now();
		node.innerHTML = html;
		const nextText = node.textContent ?? "";
		let shared = 0;
		while (shared < previousText.length && shared < nextText.length && previousText[shared] === nextText[shared]) {
			shared += 1;
		}

		// Markdown is reparsed on every update. Carry the progress of still-visible
		// text across the new DOM rather than restarting or dropping its animation.
		ranges = ranges
			.filter((range) => range.start < shared && now - range.started < animationDuration)
			.map((range) => ({ ...range, end: Math.min(range.end, shared) }));
		if (streaming && nextText.length > shared) {
			ranges.push({ start: shared, end: nextText.length, started: now });
		}
		previousText = nextText;
		if (!ranges.length) return;

		const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
		const textNodes: Text[] = [];
		while (walker.nextNode()) textNodes.push(walker.currentNode as Text);
		let offset = 0;
		for (const text of textNodes) {
			const end = offset + text.length;
			const overlapping = ranges.filter((range) => range.start < end && range.end > offset);
			if (overlapping.length && text.data.trim()) {
				const fragment = document.createDocumentFragment();
				let position = 0;
				for (const range of overlapping) {
					const start = Math.max(range.start - offset, 0);
					const finish = Math.min(range.end - offset, text.length);
					if (start > position) fragment.append(text.data.slice(position, start));
					const span = document.createElement("span");
					span.className = "message-append";
					span.style.animationDelay = `${-(now - range.started)}ms`;
					span.textContent = text.data.slice(start, finish);
					fragment.append(span);
					position = finish;
				}
				if (position < text.length) fragment.append(text.data.slice(position));
				text.replaceWith(fragment);
			}
			offset = end;
		}
	}
	update(initial);
	return { update };
}
