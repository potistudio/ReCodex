import type { Item, ServerEvent } from "./types";

export function updateItems(items: Item[], event: ServerEvent): Item[] {
	const { method, params } = event;
	if (
		(method === "item/started" || method === "item/completed") &&
		params.item
	) {
		const item = params.item;
		const index = items.findIndex((entry) => entry.id === item.id);
		if (index < 0)
			return [
				...items.filter(
					(entry) =>
						!(
							entry.id.startsWith("pending-") &&
							item.type === "userMessage"
						),
				),
				item,
			];
		return items.map((entry, i) => (i === index ? item : entry));
	}
	if (method === "item/agentMessage/delta" && params.itemId) {
		const index = items.findIndex((entry) => entry.id === params.itemId);
		if (index < 0)
			return [
				...items,
				{
					id: params.itemId,
					type: "agentMessage",
					text: params.delta ?? "",
				},
			];
		return items.map((entry, i) =>
			i === index
				? { ...entry, text: (entry.text ?? "") + (params.delta ?? "") }
				: entry,
		);
	}
	if (method === "item/commandExecution/outputDelta") {
		return items.map((entry) =>
			entry.id === params.itemId
				? {
						...entry,
						aggregatedOutput:
							(entry.aggregatedOutput ?? "") +
							(params.delta ?? ""),
					}
				: entry,
		);
	}
	return items;
}
