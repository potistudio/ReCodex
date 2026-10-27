type MarkdownState = { html: string; streaming: boolean | undefined };

// Animate only the new text, even when Markdown reparsing replaces the DOM.
// The caller must sanitize the HTML before passing it to this action.
export function streamingMarkdown(node: HTMLElement, initial: MarkdownState) {
	let previousText = "";
	function update({ html, streaming }: MarkdownState) {
		node.innerHTML = html;
		const nextText = node.textContent ?? "";
		let shared = 0;
		while (shared < previousText.length && shared < nextText.length && previousText[shared] === nextText[shared]) {
			shared += 1;
		}
		previousText = nextText;
		if (!streaming) return;

		const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
		const textNodes: Text[] = [];
		while (walker.nextNode()) textNodes.push(walker.currentNode as Text);
		let offset = 0;
		for (const text of textNodes) {
			const end = offset + text.length;
			if (end > shared && text.data.trim()) {
				const added = shared > offset ? text.splitText(shared - offset) : text;
				const span = document.createElement("span");
				span.className = "message-append";
				added.replaceWith(span);
				span.append(added);
			}
			offset = end;
		}
	}
	update(initial);
	return { update };
}
