import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { updateItems } from "./conversation";
import type { Item, Model, Project, ServerEvent, Thread, ThreadTokenUsage, TokenUsageBreakdown, Turn } from "./types";

export const rpc = <T>(method: string, params: Record<string, unknown> = {}) =>
	invoke<T>("server_request", { method, params });

export class App {
	connected = $state(false);
	connecting = $state(false);
	loading = $state(false);
	sending = $state(false);
	error = $state("");
	projects = $state<Project[]>([]);
	project = $state<Project | null>(null);
	threads = $state<Thread[]>([]);
	cursor = $state<string | null>(null);
	thread = $state<Thread | null>(null);
	items = $state<Item[]>([]);
	models = $state<Model[]>([]);
	model = $state("");
	effort = $state("");
	private itemsByThread = $state<Record<string, Item[]>>({});
	private activeTurnsByThread = $state<Record<string, string>>({});
	private approvalsByThread = $state<Record<string, ServerEvent[]>>({});
	private thinkingLabelsByThread = $state<Record<string, { itemId: string; summaryIndex: number; text: string }>>({});
	private configurationsByThread = $state<Record<string, { model: string; effort: string }>>({});
	private tokenUsageByThread = $state<Record<string, ThreadTokenUsage>>({});
	private tokenUsageByTurn = $state<Record<string, TokenUsageBreakdown>>({});
	activeTurn = $derived(this.thread ? (this.activeTurnsByThread[this.thread.id] ?? null) : null);
	approvals = $derived(this.thread ? (this.approvalsByThread[this.thread.id] ?? []) : []);
	thinkingLabel = $derived(this.thread ? (this.thinkingLabelsByThread[this.thread.id]?.text ?? "") : "");
	account = $state<{
		type: string;
		email?: string;
		planType?: string;
	} | null>(null);
	requiresAuth = $state(true);
	logs = $state<string[]>([]);
	fileRevision = $state(0);
	busy = $derived(this.sending || this.activeTurn !== null || this.loading);
	selectedModel = $derived(this.models.find((model) => model.model === this.model));
	private unlisten?: UnlistenFn;
	private disposed = false;
	private alternativeTurnIds = new Set<string>();
	private navigationRevision = 0;

	async guard(action: () => Promise<unknown>) {
		this.error = "";
		try {
			await action();
		} catch (error) {
			this.error = String(error);
		}
	}
	async init() {
		if (!isTauri()) return;
		await this.guard(async () => {
			const unlisten = await listen<ServerEvent>("codex-event", ({ payload }) => this.onEvent(payload));
			if (this.disposed) {
				unlisten();
				return;
			}
			this.unlisten = unlisten;
			this.projects = await invoke<Project[]>("projects_load");
			const saved = localStorage.getItem("recodex-project");
			this.project = this.projects.find((project) => project.path === saved) ?? this.projects[0] ?? null;
			await this.connect();
		});
	}
	dispose() {
		this.disposed = true;
		this.unlisten?.();
	}
	async connect() {
		if (this.connecting) return;
		this.connecting = true;
		await this.guard(async () => {
			await invoke("server_connect");
			this.connected = true;
			const models: Model[] = [];
			let cursor: string | null = null;
			do {
				const page: { data: Model[]; nextCursor: string | null } = await rpc("model/list", { cursor });
				models.push(...page.data.filter((model) => !model.hidden));
				cursor = page.nextCursor;
			} while (cursor);
			this.models = models;
			const saved = localStorage.getItem("recodex-model");
			this.selectModel(
				models.find((model) => model.model === saved)?.model ??
					models.find((model) => model.isDefault)?.model ??
					models[0]?.model ??
					"",
			);
			await this.readAccount();
			await this.loadThreads();
			if (this.thread) await this.resume(this.thread);
		});
		this.connecting = false;
	}
	async readAccount() {
		const result = await rpc<{
			account: { type: string; email?: string; planType?: string } | null;
			requiresOpenaiAuth: boolean;
		}>("account/read");
		this.account = result.account;
		this.requiresAuth = result.requiresOpenaiAuth;
	}
	async login() {
		await this.guard(async () => {
			const result = await rpc<{ authUrl: string }>("account/login/start", { type: "chatgpt" });
			await openUrl(result.authUrl);
		});
	}
	selectModel(model: string) {
		this.model = model;
		this.effort = this.models.find((entry) => entry.model === model)?.defaultReasoningEffort ?? "";
		localStorage.setItem("recodex-model", model);
	}
	async loadThreads(more = false) {
		if (!this.project || !this.connected) {
			this.threads = [];
			return;
		}
		const project = this.project;
		const paths = [...new Set([project.path, ...project.historyPaths])];
		const results = await Promise.all(
			paths.map((cwd, index) =>
				rpc<{ data: Thread[]; nextCursor: string | null }>("thread/list", {
					cwd,
					limit: 50,
					sortKey: "updated_at",
					cursor: more && index === 0 ? this.cursor : null,
				}),
			),
		);
		if (this.project?.path !== project.path) return;
		const threads = results.flatMap((result) => result.data);
		this.threads = more
			? [...this.threads, ...threads.filter((thread) => !this.threads.some((entry) => entry.id === thread.id))]
			: [...new Map(threads.map((thread) => [thread.id, thread])).values()].sort(
					(a, b) => b.updatedAt - a.updatedAt,
				);
		this.cursor = results[0].nextCursor;
	}
	async chooseProject(project: Project) {
		if (this.busy) return;
		this.newChat();
		this.project = project;
		localStorage.setItem("recodex-project", project.path);
		this.loading = true;
		await this.guard(() => this.loadThreads());
		this.loading = false;
	}
	async addProject() {
		if (!isTauri() || this.busy) return;
		await this.guard(async () => {
			const path = await open({
				directory: true,
				multiple: false,
				title: "Open a project",
			});
			if (!path) return;
			this.projects = await invoke<Project[]>("project_save", { path });
			await this.chooseProject(
				this.projects.find((project) => project.path === path) ?? this.projects[this.projects.length - 1],
			);
		});
	}
	async renameProject(name: string) {
		if (!this.project || !name.trim()) return;
		const path = this.project.path;
		await this.guard(async () => {
			this.projects = await invoke<Project[]>("project_save", {
				path,
				name: name.trim(),
			});
			this.project = this.projects.find((project) => project.path === path) ?? null;
		});
	}
	async relocateProject() {
		const project = this.project;
		if (!isTauri() || !project || this.busy) return;
		await this.guard(async () => {
			const path = await open({
				directory: true,
				multiple: false,
				title: "Choose a working directory",
			});
			if (!path || path === project.path) return;
			const relocated = await invoke<Project>("project_relocate", {
				sourcePath: project.path,
				destinationPath: path,
			});
			this.projects = await invoke<Project[]>("projects_load");
			await this.chooseProject(relocated);
		});
	}
	async removeProject() {
		const project = this.project;
		if (!project || this.busy) return;
		await this.guard(async () => {
			this.projects = await invoke<Project[]>("project_remove", {
				path: project.path,
			});
			this.newChat();
			this.project = null;
			this.threads = [];
			if (this.projects[0]) await this.chooseProject(this.projects[0]);
			else localStorage.removeItem("recodex-project");
		});
	}
	newChat() {
		this.navigationRevision += 1;
		this.thread = null;
		this.items = [];
		this.error = "";
	}
	async resume(thread: Thread) {
		if (this.selectRunningThread(thread)) return;
		if (this.loading) return;
		const navigationRevision = ++this.navigationRevision;
		this.loading = true;
		await this.guard(async () => {
			const result = await rpc<{
				thread: Thread;
				model: string;
				reasoningEffort: string | null;
			}>("thread/resume", {
				threadId: thread.id,
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
			});
			const items = this.mergeLoadedItems(
				result.thread.turns.flatMap((turn) => turn.items),
				this.itemsByThread[result.thread.id] ?? [],
			);
			this.setThreadItems(result.thread.id, items);
			const configuration = this.configurationsByThread[result.thread.id] ?? {
				model: result.model,
				effort: result.reasoningEffort ?? "",
			};
			if (!this.configurationsByThread[result.thread.id])
				this.setThreadConfiguration(result.thread.id, configuration);
			const activeTurn = result.thread.turns.find((turn) => turn.status === "inProgress")?.id;
			if (activeTurn) this.setActiveTurn(result.thread.id, activeTurn);
			if (navigationRevision !== this.navigationRevision) return;
			this.thread = result.thread;
			this.items = items;
			this.applyThreadConfiguration(result.thread.id);
		});
		this.loading = false;
	}
	async send(text: string): Promise<boolean> {
		if (!text.trim() || !this.project || !this.connected || this.busy) return false;
		this.sending = true;
		this.error = "";
		const pendingId = `pending-${crypto.randomUUID()}`;
		let thread = this.thread;
		try {
			const project = this.project;
			const navigationRevision = this.navigationRevision;
			if (!thread) {
				const result = await rpc<{ thread: Thread }>("thread/start", {
					cwd: project.path,
					model: this.model || null,
					approvalPolicy: "on-request",
					sandbox: "workspace-write",
				});
				thread = result.thread;
				if (navigationRevision === this.navigationRevision && !this.thread) this.thread = thread;
			}
			this.setThreadItems(thread.id, [
				...(this.itemsByThread[thread.id] ?? []),
				{
					id: pendingId,
					type: "userMessage",
					content: [{ type: "text", text }],
				},
			]);
			this.setThreadConfiguration(thread.id, { model: this.model, effort: this.effort });
			const result = await rpc<{ turn: Turn }>("turn/start", {
				threadId: thread.id,
				input: [{ type: "text", text }],
				model: this.model || null,
				effort: this.effort || null,
				summary: "concise",
			});
			// A very short turn may already have completed before the request resolves.
			if (!this.completedTurns.has(result.turn.id)) this.setActiveTurn(thread.id, result.turn.id);
			if (!thread.preview) thread.preview = text;
			await this.guard(() => this.loadThreads());
			return true;
		} catch (error) {
			this.error = String(error);
			if (thread)
				this.setThreadItems(
					thread.id,
					(this.itemsByThread[thread.id] ?? []).filter((item) => item.id !== pendingId),
				);
			return false;
		} finally {
			this.sending = false;
		}
	}
	private completedTurns = new Set<string>();
	async stop() {
		const thread = this.thread;
		const turnId = this.activeTurn;
		if (thread && turnId)
			await this.guard(() =>
				rpc("turn/interrupt", {
					threadId: thread.id,
					turnId,
				}),
			);
	}
	async respond(event: ServerEvent, result: unknown) {
		await this.guard(async () => {
			await invoke("server_respond", { id: event.id, result });
			const threadId = event.params.threadId;
			if (threadId)
				this.setApprovals(
					threadId,
					(this.approvalsByThread[threadId] ?? []).filter((entry) => entry.id !== event.id),
				);
		});
	}
	async requestAlternative(event: ServerEvent) {
		const turnId = event.params.turnId;
		if (turnId) this.alternativeTurnIds.add(turnId);
		await this.respond(event, { decision: "decline" });
	}
	private onEvent(event: ServerEvent) {
		const { method, params } = event;
		if (method === "recodex/disconnected") {
			this.connected = false;
			this.activeTurnsByThread = {};
			this.approvalsByThread = {};
			this.error = "Codex disconnected. Reconnect to continue.";
			return;
		}
		if (method === "recodex/log") {
			this.logs = [...this.logs.slice(-49), params.message ?? ""];
			return;
		}
		if (method === "account/login/completed" || method === "account/updated") {
			if (params.success === false) this.error = params.error?.message ?? "Sign in failed";
			else void this.guard(() => this.readAccount());
			return;
		}
		if (event.id !== undefined) {
			if (
				[
					"item/commandExecution/requestApproval",
					"item/fileChange/requestApproval",
					"item/tool/requestUserInput",
					"item/permissions/requestApproval",
				].includes(method)
			) {
				if (params.threadId)
					this.setApprovals(params.threadId, [...(this.approvalsByThread[params.threadId] ?? []), event]);
			} else {
				void this.guard(() =>
					invoke("server_respond", {
						id: event.id,
						error: {
							code: -32601,
							message: `ReCodex does not support ${method}`,
						},
					}),
				);
			}
			return;
		}
		if (method === "serverRequest/resolved")
			for (const [threadId, approvals] of Object.entries(this.approvalsByThread))
				this.setApprovals(
					threadId,
					approvals.filter((entry) => entry.id !== params.requestId),
				);
		if (!params.threadId) return;
		const threadId = params.threadId;
		if (method === "thread/tokenUsage/updated" && params.tokenUsage) {
			this.setThreadTokenUsage(threadId, params.turnId, params.tokenUsage);
			return;
		}
		if (method === "item/reasoning/summaryTextDelta" && params.itemId) {
			this.updateThinkingLabel(threadId, params.itemId, params.summaryIndex ?? 0, params.delta ?? "");
			return;
		}
		if (
			method === "turn/started" ||
			method === "item/agentMessage/delta" ||
			(method === "item/started" && params.item?.type !== "reasoning") ||
			(method === "item/completed" && params.item?.type !== "reasoning") ||
			method.startsWith("item/commandExecution/") ||
			method.startsWith("item/fileChange/") ||
			method.startsWith("item/mcpToolCall/")
		)
			this.clearThinkingLabel(threadId);
		this.setThreadItems(threadId, updateItems(this.itemsByThread[threadId] ?? [], event));
		if (method === "turn/started" && params.turn) this.setActiveTurn(threadId, params.turn.id);
		if (method === "turn/completed" && params.turn) {
			const turn = params.turn;
			this.clearThinkingLabel(threadId);
			this.completedTurns.add(turn.id);
			this.clearActiveTurn(threadId, turn.id);
			this.setApprovals(
				threadId,
				(this.approvalsByThread[threadId] ?? []).filter((entry) => entry.params.turnId !== turn.id),
			);
			const shouldRequestAlternative = this.alternativeTurnIds.delete(turn.id) && turn.status === "completed";
			if (turn.error) this.error = turn.error.message;
			this.fileRevision += 1;
			void this.loadThreads().catch((error) => {
				if (!this.error) this.error = String(error);
			});
			if (shouldRequestAlternative)
				void this.sendToThread(
					threadId,
					"The user declined the previous permission request. Continue with an alternative that stays within the current permissions, and do not request that permission again. If no alternative is viable, explain the blocker and the smallest safe next step.",
				);
		}
		if (method === "error") this.error = params.error?.message ?? "The turn encountered an error.";
		if (method === "item/completed" && params.item?.type === "fileChange") this.fileRevision += 1;
	}
	isThreadRunning(threadId: string) {
		return this.activeTurnsByThread[threadId] !== undefined;
	}
	tokenUsageFor(threadId: string) {
		return this.tokenUsageByThread[threadId] ?? null;
	}
	tokenUsageForTurn(turnId: string) {
		return this.tokenUsageByTurn[turnId] ?? null;
	}
	private selectRunningThread(thread: Thread) {
		const items = this.itemsByThread[thread.id];
		if (!this.isThreadRunning(thread.id) || !items) return false;
		this.navigationRevision += 1;
		this.thread = thread;
		this.items = items;
		this.applyThreadConfiguration(thread.id);
		return true;
	}
	private setThreadItems(threadId: string, items: Item[]) {
		this.itemsByThread = { ...this.itemsByThread, [threadId]: items };
		if (this.thread?.id === threadId) this.items = items;
	}
	private setActiveTurn(threadId: string, turnId: string) {
		this.activeTurnsByThread = { ...this.activeTurnsByThread, [threadId]: turnId };
	}
	private clearActiveTurn(threadId: string, turnId: string) {
		if (this.activeTurnsByThread[threadId] !== turnId) return;
		const { [threadId]: _, ...activeTurns } = this.activeTurnsByThread;
		this.activeTurnsByThread = activeTurns;
	}
	private setApprovals(threadId: string, approvals: ServerEvent[]) {
		this.approvalsByThread = { ...this.approvalsByThread, [threadId]: approvals };
	}
	private updateThinkingLabel(threadId: string, itemId: string, summaryIndex: number, delta: string) {
		const previous = this.thinkingLabelsByThread[threadId];
		const text =
			`${previous?.itemId === itemId && previous.summaryIndex === summaryIndex ? previous.text : ""}${delta}`
				.replaceAll("**", "")
				.trimStart();
		this.thinkingLabelsByThread = { ...this.thinkingLabelsByThread, [threadId]: { itemId, summaryIndex, text } };
	}
	private clearThinkingLabel(threadId: string) {
		if (!(threadId in this.thinkingLabelsByThread)) return;
		const { [threadId]: _, ...thinkingLabels } = this.thinkingLabelsByThread;
		this.thinkingLabelsByThread = thinkingLabels;
	}
	private setThreadConfiguration(threadId: string, configuration: { model: string; effort: string }) {
		this.configurationsByThread = { ...this.configurationsByThread, [threadId]: configuration };
	}
	private setThreadTokenUsage(threadId: string, turnId: string | undefined, tokenUsage: ThreadTokenUsage) {
		this.tokenUsageByThread = { ...this.tokenUsageByThread, [threadId]: tokenUsage };
		if (turnId) this.tokenUsageByTurn = { ...this.tokenUsageByTurn, [turnId]: tokenUsage.last };
	}
	private applyThreadConfiguration(threadId: string) {
		const configuration = this.configurationsByThread[threadId];
		if (!configuration) return;
		if (this.models.some((model) => model.model === configuration.model)) this.selectModel(configuration.model);
		this.effort = configuration.effort;
	}
	private mergeLoadedItems(loaded: Item[], cached: Item[]) {
		const cachedById = new Map(cached.map((item) => [item.id, item]));
		const items = loaded.map((item) => {
			const previous = cachedById.get(item.id);
			cachedById.delete(item.id);
			return previous
				? {
						...item,
						...(item.text === undefined && previous.text !== undefined ? { text: previous.text } : {}),
						...(item.aggregatedOutput === undefined && previous.aggregatedOutput !== undefined
							? { aggregatedOutput: previous.aggregatedOutput }
							: {}),
					}
				: item;
		});
		return [...items, ...cachedById.values()];
	}
	private async sendToThread(threadId: string, text: string) {
		if (!this.connected) return;
		const pendingId = `pending-${crypto.randomUUID()}`;
		this.setThreadItems(threadId, [
			...(this.itemsByThread[threadId] ?? []),
			{
				id: pendingId,
				type: "userMessage",
				content: [{ type: "text", text }],
			},
		]);
		try {
			const configuration = this.configurationsByThread[threadId] ?? {
				model: this.model,
				effort: this.effort,
			};
			const result = await rpc<{ turn: Turn }>("turn/start", {
				threadId,
				input: [{ type: "text", text }],
				model: configuration.model || null,
				effort: configuration.effort || null,
				summary: "concise",
			});
			if (!this.completedTurns.has(result.turn.id)) this.setActiveTurn(threadId, result.turn.id);
		} catch (error) {
			this.setThreadItems(
				threadId,
				(this.itemsByThread[threadId] ?? []).filter((item) => item.id !== pendingId),
			);
			this.error = String(error);
		}
	}
}
