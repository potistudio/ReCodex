export interface Project {
	name: string;
	path: string;
}
export interface Model {
	id: string;
	model: string;
	displayName: string;
	description: string;
	isDefault: boolean;
	hidden: boolean;
	defaultReasoningEffort: string;
	supportedReasoningEfforts: {
		reasoningEffort: string;
		description: string;
	}[];
}
export interface Item {
	id: string;
	type: string;
	text?: string;
	content?: { type: string; text?: string }[];
	summary?: string[];
	command?: string;
	aggregatedOutput?: string;
	status?: string;
	changes?: { path: string; kind: { type: string }; diff: string }[];
	tool?: string;
	server?: string;
}
export interface Turn {
	id: string;
	items: Item[];
	status: string;
	error?: { message: string } | null;
}
export interface Thread {
	id: string;
	name: string | null;
	preview: string;
	cwd: string;
	updatedAt: number;
	turns: Turn[];
}
export interface Question {
	id: string;
	header: string;
	question: string;
	options?: { label: string; description: string }[] | null;
	isSecret?: boolean;
}
export interface ServerEvent {
	id?: number | string;
	method: string;
	params: {
		threadId?: string;
		turnId?: string;
		itemId?: string;
		delta?: string;
		output?: string;
		item?: Item;
		turn?: Turn;
		thread?: Thread;
		requestId?: number | string;
		message?: string;
		error?: { message: string };
		success?: boolean;
		reason?: string;
		command?: string;
		cwd?: string;
		grantRoot?: string;
		questions?: Question[];
		permissions?: Record<string, unknown>;
		availableDecisions?: unknown[];
		networkApprovalContext?: { host: string; protocol: string };
	};
}
export interface FileEntry {
	name: string;
	path: string;
	isDirectory: boolean;
}
