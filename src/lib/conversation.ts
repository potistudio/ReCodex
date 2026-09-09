import type { Item, ServerEvent } from "./types";

function mergeItem(previous: Item | undefined, item: Item): Item {
	if (!previous) return item;
	const aggregatedOutput = item.aggregatedOutput ?? previous.aggregatedOutput;
	return {
		...previous,
		...item,
		...(aggregatedOutput === undefined ? {} : { aggregatedOutput }),
	};
}

export function updateItems(items: Item[], event: ServerEvent): Item[] {
	const { method, params } = event;
	if ((method === "item/started" || method === "item/updated" || method === "item/completed") && params.item) {
		const item = params.item;
		const index = items.findIndex((entry) => entry.id === item.id);
		if (index < 0)
			return [
				...items.filter((entry) => !(entry.id.startsWith("pending-") && item.type === "userMessage")),
				item,
			];
		return items.map((entry, i) => (i === index ? mergeItem(entry, item) : entry));
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
			i === index ? { ...entry, text: (entry.text ?? "") + (params.delta ?? "") } : entry,
		);
	}
	if (method === "item/commandExecution/outputDelta") {
		if (!params.itemId) return items;
		const index = items.findIndex((entry) => entry.id === params.itemId);
		if (index < 0)
			return [
				...items,
				{
					id: params.itemId,
					type: "commandExecution",
					status: "inProgress",
					aggregatedOutput: params.delta ?? "",
				},
			];
		return items.map((entry) =>
			entry.id === params.itemId
				? {
						...entry,
						aggregatedOutput: (entry.aggregatedOutput ?? "") + (params.delta ?? ""),
					}
				: entry,
		);
	}
	if (method === "item/commandExecution/outputCompleted" && params.itemId) {
		return items.map((entry) =>
			entry.id === params.itemId && params.output !== undefined
				? { ...entry, aggregatedOutput: params.output }
				: entry,
		);
	}
	return items;
}
