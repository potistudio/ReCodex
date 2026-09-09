import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { updateItems } from "./conversation";
import type { Item, Model, Project, ServerEvent, Thread, Turn } from "./types";

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
	activeTurn = $state<string | null>(null);
	approvals = $state<ServerEvent[]>([]);
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
		const projectPath = this.project.path;
		const result = await rpc<{ data: Thread[]; nextCursor: string | null }>("thread/list", {
			cwd: projectPath,
			limit: 50,
			sortKey: "updated_at",
			cursor: more ? this.cursor : null,
		});
		if (this.project?.path !== projectPath) return;
		this.threads = more
			? [
					...this.threads,
					...result.data.filter((thread) => !this.threads.some((entry) => entry.id === thread.id)),
				]
			: result.data;
		this.cursor = result.nextCursor;
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
		if (this.busy) return;
		this.thread = null;
		this.items = [];
		this.error = "";
		this.approvals = [];
	}
	async resume(thread: Thread) {
		if (this.busy) return;
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
			this.thread = result.thread;
			this.items = result.thread.turns.flatMap((turn) => turn.items);
			if (this.models.some((model) => model.model === result.model)) this.selectModel(result.model);
			if (result.reasoningEffort) this.effort = result.reasoningEffort;
			this.activeTurn = result.thread.turns.find((turn) => turn.status === "inProgress")?.id ?? null;
		});
		this.loading = false;
	}
	async send(text: string): Promise<boolean> {
		if (!text.trim() || !this.project || !this.connected || this.busy) return false;
		this.sending = true;
		this.error = "";
		const pendingId = `pending-${crypto.randomUUID()}`;
		try {
			if (!this.thread) {
				const result = await rpc<{ thread: Thread }>("thread/start", {
					cwd: this.project.path,
					model: this.model || null,
					approvalPolicy: "on-request",
					sandbox: "workspace-write",
				});
				this.thread = result.thread;
			}
			this.items = [
				...this.items,
				{
					id: pendingId,
					type: "userMessage",
					content: [{ type: "text", text }],
				},
			];
			const result = await rpc<{ turn: Turn }>("turn/start", {
				threadId: this.thread.id,
				input: [{ type: "text", text }],
				model: this.model || null,
				effort: this.effort || null,
			});
			// A very short turn may already have completed before the request resolves.
			if (!this.completedTurns.has(result.turn.id)) this.activeTurn = result.turn.id;
			if (!this.thread.preview) this.thread.preview = text;
			await this.guard(() => this.loadThreads());
			return true;
		} catch (error) {
			this.error = String(error);
			this.items = this.items.filter((item) => item.id !== pendingId);
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
			this.approvals = this.approvals.filter((entry) => entry.id !== event.id);
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
			this.activeTurn = null;
			this.approvals = [];
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
				this.approvals = [...this.approvals, event];
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
			this.approvals = this.approvals.filter((entry) => entry.id !== params.requestId);
		if (!params.threadId || params.threadId !== this.thread?.id) return;
		this.items = updateItems(this.items, event);
		if (method === "turn/started" && params.turn) this.activeTurn = params.turn.id;
		if (method === "turn/completed" && params.turn) {
			const turn = params.turn;
			this.completedTurns.add(turn.id);
			this.activeTurn = null;
			this.approvals = this.approvals.filter((entry) => entry.params.turnId !== turn.id);
			const shouldRequestAlternative = this.alternativeTurnIds.delete(turn.id) && turn.status === "completed";
			if (turn.error) this.error = turn.error.message;
			this.fileRevision += 1;
			void this.loadThreads().catch((error) => {
				if (!this.error) this.error = String(error);
			});
			if (shouldRequestAlternative)
				void this.send(
					"The user declined the previous permission request. Continue with an alternative that stays within the current permissions, and do not request that permission again. If no alternative is viable, explain the blocker and the smallest safe next step.",
				);
		}
		if (method === "error") this.error = params.error?.message ?? "The turn encountered an error.";
		if (method === "item/completed" && params.item?.type === "fileChange") this.fileRevision += 1;
	}
}
