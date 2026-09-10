<script lang="ts">
import {
	ArrowUp,
	ArrowUpRight,
	Check,
	ChevronDown,
	Code2,
	FileCode,
	Folder,
	FolderPlus,
	LoaderCircle,
	Moon,
	MoreHorizontal,
	PanelLeftClose,
	PanelLeftOpen,
	Pencil,
	Plus,
	Search,
	Settings2,
	ShieldCheck,
	Sparkles,
	Square,
	SquarePen,
	Sun,
	X,
} from "@lucide/svelte";
import { isTauri } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { onMount, tick } from "svelte";
import { App } from "$lib/app.svelte";
import Approval from "$lib/components/Approval.svelte";
import FilePanel from "$lib/components/FilePanel.svelte";
import Message from "$lib/components/Message.svelte";
import { Button } from "$lib/components/ui/button";
import * as Dialog from "$lib/components/ui/dialog";
import * as Dropdown from "$lib/components/ui/dropdown-menu";
import { Input } from "$lib/components/ui/input";
import type { Project, RateLimitSnapshot, RateLimitWindow } from "$lib/types";
import { formatResetTime, formatTokenCount, formatUsageWindow, remainingPercent } from "$lib/usage";
import { workingState } from "$lib/working";

const app = new App();
let draft = $state("");
let search = $state("");
let searchOpen = $state(false);
let sidebar = $state(true);
let filesOpen = $state(false);
let fileDirty = $state(false);
let settingsOpen = $state(false);
let renameOpen = $state(false);
let projectName = $state("");
let dark = $state(false);
let chatScroll = $state<HTMLDivElement>();
let composer = $state<HTMLTextAreaElement>();
let scrollMode = $state<"follow" | "free">("follow");
let manualScroll = false;
let wheelTimer: ReturnType<typeof setTimeout> | undefined;
let labelTransitionTimer: ReturnType<typeof setTimeout> | undefined;
const labelTransitionDuration = 600;
const activeWork = $derived(workingState(app.items, app.approvals.length > 0, app.thinkingLabel));
let displayedWorkLabel = $state("");
let outgoingWorkLabel = $state("");
let pendingWorkLabels = $state<string[]>([]);
const displayedWorkTokens = $derived(labelTokens(displayedWorkLabel));
const outgoingWorkTokens = $derived(labelTokens(outgoingWorkLabel));
const workTokenCount = $derived(Math.max(displayedWorkTokens.length, outgoingWorkTokens.length));
const filteredThreads = $derived(
	app.threads.filter((thread) => (thread.name || thread.preview).toLowerCase().includes(search.toLowerCase())),
);
const ready = $derived(app.connected && !!app.project && (!app.requiresAuth || !!app.account));
const rateLimitSnapshots = $derived.by(() => {
	const rateLimits = app.rateLimits;
	if (!rateLimits) return [];
	const snapshots = [rateLimits.rateLimits, ...Object.values(rateLimits.rateLimitsByLimitId ?? {})].filter(
		(snapshot): snapshot is RateLimitSnapshot => Boolean(snapshot),
	);
	return [...new Map(snapshots.map((snapshot) => [snapshot.limitId ?? "default", snapshot])).values()];
});
function rateLimitName(snapshot: RateLimitSnapshot) {
	return snapshot.limitName ?? snapshot.limitId ?? "Codex";
}
function rateLimitWindows(snapshot: RateLimitSnapshot): RateLimitWindow[] {
	return [snapshot.primary, snapshot.secondary].filter((window): window is RateLimitWindow => window !== null);
}
function labelTokens(label: string) {
	return label.match(/\S+\s*/g) ?? [];
}
function startWorkLabelTransition(nextLabel: string) {
	const previousLabel = displayedWorkLabel;
	displayedWorkLabel = nextLabel;
	if (!previousLabel) return;
	outgoingWorkLabel = previousLabel;
	labelTransitionTimer = setTimeout(() => {
		outgoingWorkLabel = "";
		labelTransitionTimer = undefined;
		const [nextPendingLabel, ...remainingLabels] = pendingWorkLabels;
		pendingWorkLabels = remainingLabels;
		if (nextPendingLabel) startWorkLabelTransition(nextPendingLabel);
	}, labelTransitionDuration);
}
onMount(() => {
	dark = localStorage.getItem("recodex-theme") === "dark";
	void app.init();
	return () => {
		if (labelTransitionTimer) clearTimeout(labelTransitionTimer);
		app.dispose();
	};
});
$effect(() => {
	document.documentElement.classList.toggle("dark", dark);
	localStorage.setItem("recodex-theme", dark ? "dark" : "light");
});
$effect(() => {
	const nextLabel = activeWork.label;
	if (!app.thinkingLabel) {
		pendingWorkLabels = [];
		if (labelTransitionTimer) clearTimeout(labelTransitionTimer);
		labelTransitionTimer = undefined;
		outgoingWorkLabel = "";
		displayedWorkLabel = nextLabel;
		return;
	}
	if (nextLabel === displayedWorkLabel || pendingWorkLabels.includes(nextLabel)) return;
	if (outgoingWorkLabel) {
		pendingWorkLabels = [...pendingWorkLabels, nextLabel];
		return;
	}
	startWorkLabelTransition(nextLabel);
});
$effect(() => {
	app.items;
	app.activeTurn;
	app.approvals;
	if (scrollMode === "follow" && app.items.length)
		void tick().then(() => {
			if (chatScroll) chatScroll.scrollTop = chatScroll.scrollHeight;
		});
});
function updateScrollMode() {
	if (!chatScroll || !manualScroll) return;
	scrollMode = chatScroll.scrollHeight - chatScroll.scrollTop - chatScroll.clientHeight < 100 ? "follow" : "free";
}
function beginWheelScroll() {
	manualScroll = true;
	if (wheelTimer) clearTimeout(wheelTimer);
	wheelTimer = setTimeout(() => {
		manualScroll = false;
	}, 150);
}
function trackManualScroll(node: HTMLElement) {
	const beginPointerScroll = () => {
		manualScroll = true;
	};
	const endPointerScroll = () => {
		manualScroll = false;
	};
	node.addEventListener("wheel", beginWheelScroll, { passive: true });
	node.addEventListener("pointerdown", beginPointerScroll);
	node.addEventListener("touchstart", beginPointerScroll, { passive: true });
	window.addEventListener("pointerup", endPointerScroll);
	window.addEventListener("touchend", endPointerScroll);
	return {
		destroy() {
			if (wheelTimer) clearTimeout(wheelTimer);
			node.removeEventListener("wheel", beginWheelScroll);
			node.removeEventListener("pointerdown", beginPointerScroll);
			node.removeEventListener("touchstart", beginPointerScroll);
			window.removeEventListener("pointerup", endPointerScroll);
			window.removeEventListener("touchend", endPointerScroll);
		},
	};
}
function followLatest() {
	scrollMode = "follow";
	void tick().then(() => chatScroll?.scrollTo({ behavior: "smooth", top: chatScroll.scrollHeight }));
}
function centerLatestUserMessage() {
	if (!chatScroll) return;
	const messages = chatScroll.querySelectorAll<HTMLElement>(".user-message");
	const message = messages[messages.length - 1];
	if (!message) return;
	const scrollBounds = chatScroll.getBoundingClientRect();
	const messageBounds = message.getBoundingClientRect();
	chatScroll.scrollTo({
		behavior: "auto",
		top: Math.max(
			0,
			chatScroll.scrollTop +
				messageBounds.top -
				scrollBounds.top -
				(chatScroll.clientHeight - messageBounds.height) / 2,
		),
	});
}
async function send() {
	const text = draft.trim();
	if (!text) return;
	const wasFollowing = scrollMode === "follow";
	draft = "";
	if (!(await app.send(text))) {
		draft = text;
	} else if (wasFollowing) {
		scrollMode = "free";
		await tick();
		centerLatestUserMessage();
	}
	await tick();
	composer?.focus();
}
function inputKey(event: KeyboardEvent) {
	if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
		event.preventDefault();
		if (ready && !app.busy) void send();
	}
}
function useSuggestion(prompt: string) {
	draft = prompt;
	composer?.focus();
}
async function canLeaveFiles() {
	return (
		!fileDirty ||
		(await confirm("Discard unsaved file changes before switching projects?", {
			title: "Unsaved changes",
			kind: "warning",
		}))
	);
}
async function chooseProject(project: Project) {
	if (app.project?.path === project.path || app.busy) return;
	if (await canLeaveFiles()) {
		fileDirty = false;
		scrollMode = "follow";
		await app.chooseProject(project);
	}
}
function newChat() {
	scrollMode = "follow";
	app.newChat();
}
async function addProject() {
	if (await canLeaveFiles()) {
		await app.addProject();
	}
}
async function relocateProject() {
	if (await canLeaveFiles()) {
		fileDirty = false;
		await app.relocateProject();
	}
}
async function removeProject() {
	if (!(await canLeaveFiles())) return;
	if (
		await confirm("Remove this project from ReCodex? Files and Codex conversations will stay on disk.", {
			title: "Remove project",
		})
	) {
		fileDirty = false;
		await app.removeProject();
	}
}
function shortcuts(event: KeyboardEvent) {
	if ((event.ctrlKey || event.metaKey) && event.key === "k") {
		event.preventDefault();
		sidebar = true;
		searchOpen = true;
	}
	if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === "o") {
		event.preventDefault();
		newChat();
	}
}
</script>

<svelte:head
	><title>ReCodex</title>
	<meta
		name="description"
		content="A quiet workspace for your next idea. Chat, code, and create with Codex."
	></svelte:head
>
<svelte:window
	onkeydown={shortcuts}
	onbeforeunload={(event) => {
		if (fileDirty) {
			event.preventDefault();
			event.returnValue = "";
		}
	}}
/>

<div class="app-shell" class:sidebar-hidden={!sidebar}>
	{#if sidebar}
		<aside class="sidebar">
			<div class="brand-row">
				<a
					href="/"
					class="brand"
					onclick={(event) => {
						event.preventDefault();
						newChat();
					}}
					><span class="brand-mark"><Code2 size={21} strokeWidth={2.1} /></span>ReCodex</a
				><Button variant="ghost" size="icon" aria-label="Hide sidebar" onclick={() => (sidebar = false)}
					><PanelLeftClose /></Button
				>
			</div>
			<nav class="primary-nav" aria-label="Workspace">
				<button type="button" onclick={newChat} disabled={!app.connected || app.loading}>
					<SquarePen size={18} /><span>New chat</span><kbd>Ctrl ⇧ O</kbd>
				</button>
				<button type="button" onclick={() => (searchOpen = !searchOpen)}>
					<Search size={18} />
					<span>Search chats</span><kbd>Ctrl K</kbd>
				</button>
			</nav>
			{#if searchOpen}
				<div class="sidebar-search">
					<Input aria-label="Search chats" placeholder="Search this project…" bind:value={search} />
				</div>
			{/if}
			<div class="section-heading">
				<span>Projects</span
				><Button
					variant="ghost"
					size="icon-xs"
					aria-label="Add project"
					disabled={app.busy || !isTauri()}
					onclick={addProject}
					><Plus /></Button
				>
			</div>
			<div class="project-list">
				{#each app.projects as project}
					<button
						type="button"
						class:selected={app.project?.path === project.path}
						disabled={app.busy}
						onclick={() => chooseProject(project)}
						title={project.path}
					>
						<Folder size={17} />
						<span class="truncate">{project.name}</span>
						{#if app.project?.path === project.path}
							<ChevronDown size={13} />
						{/if}
					</button>
				{/each}
				<button type="button" class="add-project" disabled={app.busy || !isTauri()} onclick={addProject}>
					<FolderPlus size={17} /><span>Open a project</span>
				</button>
			</div>
			<div class="section-heading recent-heading">
				<span>Recent chats</span>
				{#if app.loading}
					<LoaderCircle class="spin" size={13} />
				{/if}
			</div>
			<div class="thread-list">
				{#each filteredThreads as thread}
					<button
						type="button"
						class:selected={app.thread?.id === thread.id}
						disabled={app.loading || !app.connected}
						onclick={() => {
							scrollMode = "follow";
							void app.resume(thread);
						}}
						title={thread.name || thread.preview}
					>
						<span class="thread-summary">
							<span class="truncate">{thread.name || thread.preview || "Untitled chat"}</span>
							{#if app.tokenUsageFor(thread.id)}
								<small title="Total tokens reported by Codex for this session"
									>{formatTokenCount(app.tokenUsageFor(thread.id).total.totalTokens)}
									tokens</small
								>
							{/if}
						</span>
						{#if app.isThreadRunning(thread.id) && app.thread?.id !== thread.id}
							<LoaderCircle class="spin thread-running" size={14} aria-hidden="true" />
						{/if}
					</button>
				{/each}
				{#if !filteredThreads.length}
					<div class="sidebar-empty">
						{search
							? "No matching chats."
							: "Your conversations will appear here."}
					</div>
				{/if}
				{#if app.cursor}
					<Button
						variant="ghost"
						size="sm"
						disabled={app.busy}
						onclick={() => app.guard(() => app.loadThreads(true))}
						>Load more</Button
					>
				{/if}
			</div>
			<div class="sidebar-footer">
				<button
					type="button"
					class="account-button"
					onclick={() => {
						settingsOpen = true;
						void app.readRateLimits().catch(() => {});
					}}
				>
					<span class="avatar">{app.account?.email?.[0]?.toUpperCase() || "R"}</span
					><span
						><strong
							>{app.account?.email?.split("@")[0] ||
								"Your workspace"}</strong
						><small
							><span class="connection-dot" class:online={app.connected}></span>
							{app.connecting
								? "Connecting…"
								: app.connected
									? "Codex connected"
								: "Not connected"}</small
						></span
					><Settings2 size={17} />
				</button>
			</div>
		</aside>
	{/if}
	<main class="main-workspace">
		<header class="topbar">
			<div class="topbar-left">
				{#if !sidebar}
					<Button variant="ghost" size="icon" aria-label="Show sidebar" onclick={() => (sidebar = true)}
						><PanelLeftOpen /></Button
					>
				{/if}
				<Dropdown.Root
					><Dropdown.Trigger class="model-trigger" disabled={!app.models.length || app.busy}
						><span>{app.selectedModel?.displayName || "Codex"}</span>
						<ChevronDown size={15} /></Dropdown.Trigger
					><Dropdown.Content align="start" class="w-80"
						><Dropdown.Label>Choose a model</Dropdown.Label>
						{#each app.models as model}
							<Dropdown.Item onclick={() => app.selectModel(model.model)} class="model-option"
								><div>
									<strong>{model.displayName}</strong><small>{model.description}</small>
								</div>
								{#if app.model === model.model}
									<Check size={15} />
								{/if}</Dropdown.Item
							>
						{/each}</Dropdown.Content
					></Dropdown.Root
				>
			</div>
			<div class="topbar-right">
				<span
					class="rate-limit-indicator"
					role="status"
					title="Remaining Codex rate-limit allowance"
					aria-label="Codex rate-limit allowance"
				>
					<span>Rate limit</span>
					<strong
						>{app.rateLimits?.rateLimits.primary
							? `${remainingPercent(app.rateLimits.rateLimits.primary.usedPercent)}% left`
							: "—"}</strong
					>
				</span>
				{#if app.project}
					<span class="project-breadcrumb"><Folder size={14} />{app.project.name}</span
					><Button
						variant="ghost"
						size="sm"
						class={filesOpen ? "files-active" : ""}
						onclick={() => (filesOpen = true)}
						><FileCode size={16} />Files</Button
					><Dropdown.Root
						><Dropdown.Trigger class="icon-button" aria-label="Project options" disabled={app.busy}
							><MoreHorizontal size={18} /></Dropdown.Trigger
						><Dropdown.Content align="end"
							><Dropdown.Item
								onclick={() => {
									const project = app.project;
									if (!project) return;
									projectName = project.name;
									renameOpen = true;
								}}
								><Pencil size={14} />Rename project</Dropdown.Item
							><Dropdown.Item onclick={relocateProject}>Change working directory</Dropdown.Item
							><Dropdown.Item onclick={removeProject}>Remove project</Dropdown.Item></Dropdown.Content
						></Dropdown.Root
					>
				{:else}
					<span class="topbar-hint">Your ideas, in progress.</span>
				{/if}
			</div>
		</header>
		<div class="workspace-body">
			<section class="chat-pane" aria-label="Chat">
				{#if !isTauri()}
					<div class="preview-banner">
						Browser preview · Open the desktop app to connect to Codex and your files.
					</div>
				{/if}
				{#if app.error}
					<div class="error-banner" role="alert">
						<span>{app.error}</span
						><Button
							variant="ghost"
							size="icon-xs"
							aria-label="Dismiss error"
							onclick={() => (app.error = "")}
							><X /></Button
						>
					</div>
				{/if}
				<div class="chat-scroll-frame">
					<div class="chat-scroll" bind:this={chatScroll} use:trackManualScroll onscroll={updateScrollMode}>
						{#if app.items.length === 0}
							<div class="welcome">
								<div class="welcome-eyebrow"><span></span>A little focus. A lot of possibility.</div>
								<h1>What will you build?</h1>
								<p>
									Bring an idea. Make it real.<br>Your code and conversations, in one quiet workspace.
								</p>
								{#if !app.project}
									<Button
										variant="outline"
										size="lg"
										onclick={addProject}
										disabled={!isTauri() || app.busy}
										><FolderPlus size={16} />Open a project<ArrowUpRight size={14} /></Button
									>
								{:else}
									<div class="workspace-pill">
										<Folder size={14} /><span>{app.project.name}</span><span class="pill-dot"></span
										><span>Ready to create</span>
									</div>
								{/if}
								<div class="suggestions">
									{#each [{ icon: Code2, title: "Explore the code", detail: "Find your way around", prompt: "Give me a concise overview of this project, its architecture, and where to start." }, { icon: Sparkles, title: "Build something", detail: "Start with a small idea", prompt: "Help me build a new feature in this project. First, ask me what I want to create." }, { icon: FileCode, title: "Make it better", detail: "A fresh pair of eyes", prompt: "Review this project for concrete bugs and suggest the most useful fixes before making changes." }] as suggestion}
										<button type="button" onclick={() => useSuggestion(suggestion.prompt)}>
											<suggestion.icon size={20} strokeWidth={1.5} />
											<strong>{suggestion.title}</strong><span>{suggestion.detail}</span>
											<ArrowUpRight size={14} class="suggestion-arrow" />
										</button>
									{/each}
								</div>
							</div>
						{:else}
							<div class="messages">
								{#each app.items as item (item.renderKey ?? item.id)}
									<Message
										{item}
										tokenUsage={item.turnId ? app.tokenUsageForTurn(item.turnId) : null}
									/>
								{/each}
								{#if app.activeTurn || app.sending}
									<div class="working">
										<span class="working-dot"></span>
										<span class="working-label">
											{#each Array(workTokenCount) as _, index (index)}
												<span class="working-label-token" style:--label-index={index}>
													{#if outgoingWorkLabel}
														<span
															class="working-label-token-text working-label-token-outgoing"
															>{outgoingWorkTokens[index] ?? "\u00a0"}</span
														>
													{/if}
													{#key displayedWorkLabel}
														<span
															class="working-label-token-text"
															class:working-label-token-incoming={outgoingWorkLabel}
															>{displayedWorkTokens[index] ?? "\u00a0"}</span
														>
													{/key}
												</span>
											{/each}
										</span>
										{#if activeWork.detail}
											<span class="working-detail">{activeWork.detail}</span>
										{/if}
									</div>
								{/if}
								{#each app.approvals as event (event.id)}
									<Approval
										{event}
										respond={(event, result) => app.respond(event, result)}
										requestAlternative={(event) => app.requestAlternative(event)}
									/>
								{/each}
								{#if app.activeTurn || app.sending || scrollMode === "free"}
									<div class="message-scroll-spacer" aria-hidden="true"></div>
								{/if}
							</div>
						{/if}
					</div>
					{#if scrollMode === "free"}
						<button
							class="scroll-to-latest"
							type="button"
							aria-label="Scroll to latest"
							onclick={followLatest}
						>
							<ChevronDown size={18} />
						</button>
					{/if}
				</div>
				<div class="composer-area">
					<form
						class="composer"
						onsubmit={(event) => {
							event.preventDefault();
							void send();
						}}
					>
						<textarea
							bind:this={composer}
							bind:value={draft}
							onkeydown={inputKey}
							aria-label="Message Codex"
							placeholder={app.project
								? "Ask Codex to build, explain, or fix anything…"
								: "What would you like to create?"}
							rows="2"
						></textarea>
						<div class="composer-toolbar">
							<div class="composer-context">
								<Folder size={14} />
								<span
									>{app.project?.name ||
										"Select a project"}</span
								>
								{#if app.selectedModel?.supportedReasoningEfforts.length}
									<span class="toolbar-divider"></span
									><select aria-label="Reasoning effort" disabled={app.busy} bind:value={app.effort}>
										{#each app.selectedModel.supportedReasoningEfforts as option}
											<option value={option.reasoningEffort}>
												{option.reasoningEffort
													.charAt(0)
													.toUpperCase() +
													option.reasoningEffort.slice(
														1,
													)}
											</option>
										{/each}
									</select>
								{/if}
							</div>
							{#if app.activeTurn}
								<Button size="icon" aria-label="Stop generation" onclick={() => app.stop()}
									><Square size={13} fill="currentColor" /></Button
								>
							{:else}
								<Button
									type="submit"
									size="icon"
									class="send-button"
									aria-label="Send message"
									disabled={!draft.trim() ||
										!ready ||
										app.busy}
									>{#if app.sending}
										<LoaderCircle class="spin" />
									{:else}
										<ArrowUp size={18} />
									{/if}</Button
								>
							{/if}
						</div>
					</form>
					<div class="composer-footnote">
						<span><ShieldCheck size={12} />Review changes before you commit</span
						><span>Shift + Enter for a new line</span>
					</div>
					{#if isTauri() && !app.connected}
						<div class="connection-action">
							<Button variant="outline" size="sm" disabled={app.connecting} onclick={() => app.connect()}
								>{app.connecting
									? "Connecting to Codex…"
									: "Connect to Codex"}</Button
							>
						</div>
					{:else if app.connected && app.requiresAuth && !app.account}
						<div class="connection-action">
							<Button size="sm" onclick={() => app.login()}>Sign in with ChatGPT</Button>
						</div>
					{/if}
				</div>
			</section>
			{#if filesOpen && app.project}
				{#key app.project.path}
					<FilePanel
						project={app.project}
						revision={app.fileRevision}
						onclose={() => (filesOpen = false)}
						ondirty={(dirty) => (fileDirty = dirty)}
					/>
				{/key}
			{/if}
		</div>
	</main>
</div>

<Dialog.Root bind:open={settingsOpen}
	><Dialog.Content class="settings-dialog"
		><Dialog.Header
			><Dialog.Title>Settings</Dialog.Title
			><Dialog.Description>A workspace that feels like yours.</Dialog.Description></Dialog.Header
		>
		<div class="settings-row">
			<span><strong>Appearance</strong><small>Choose your preferred theme</small></span
			><Button variant="outline" onclick={() => (dark = !dark)}
				>{#if dark}
					<Sun />Light
				{:else}
					<Moon />Dark
				{/if}</Button
			>
		</div>
		<div class="settings-row">
			<span
				><strong>Codex connection</strong
				><small
					>{app.connected
						? "Connected to local app-server"
						: "Not connected"}</small
				></span
			><Button variant="outline" disabled={!isTauri() || app.connecting || app.busy} onclick={() => app.connect()}
				>{app.connecting ? "Connecting…" : "Reconnect"}</Button
			>
		</div>
		<div class="settings-row">
			<span
				><strong>Account</strong
				><small
					>{app.account?.email ||
						app.account?.type ||
						"Not signed in"}</small
				></span
			>
			{#if !app.account}
				<Button disabled={!app.connected} onclick={() => app.login()}>Sign in</Button>
			{:else}
				<span class="account-plan">{app.account.planType || app.account.type}</span>
			{/if}
		</div>
		<section class="rate-limit-panel" aria-label="Rate limits">
			<div>
				<strong>Rate limits</strong>
				<small>Remaining allowance from your signed-in Codex account.</small>
			</div>
			{#if rateLimitSnapshots.length}
				<div class="rate-limit-list">
					{#each rateLimitSnapshots as snapshot (snapshot.limitId ?? "default")}
						{#each rateLimitWindows(snapshot) as window, index (`${snapshot.limitId ?? "default"}-${index}`)}
							<div class="rate-limit-row">
								<span
									><strong>{rateLimitName(snapshot)}</strong
									><small>{formatUsageWindow(window)}</small></span
								>
								<span class="rate-limit-value"
									><strong>{remainingPercent(window.usedPercent)}% remaining</strong>
									<small
										>{formatResetTime(window.resetsAt) ? `Resets ${formatResetTime(window.resetsAt)}` : "Reset unavailable"}</small
									></span
								>
							</div>
						{/each}
					{/each}
				</div>
			{:else}
				<p>Rate-limit details are unavailable for this account.</p>
			{/if}
		</section>
		<p class="settings-note">
			ReCodex uses your installed Codex CLI and its sign-in. If Codex is not found, set
			<code>RECODEX_CODEX_PATH</code>
			to the executable path and restart the app.
		</p>
		{#if app.logs.length}
			<details class="diagnostics">
				<summary>Connection diagnostics</summary>
				<pre>{app.logs.join("\n")}</pre>
			</details>
		{/if}
		<div class="settings-version"><Code2 size={14} />ReCodex <span>0.0.1-alpha</span></div></Dialog.Content
	></Dialog.Root
>
<Dialog.Root bind:open={renameOpen}
	><Dialog.Content
		><Dialog.Header
			><Dialog.Title>Rename project</Dialog.Title
			><Dialog.Description>Change the name shown in your sidebar.</Dialog.Description></Dialog.Header
		>
		<form
			onsubmit={async (event) => {
				event.preventDefault();
				await app.renameProject(projectName);
				if (!app.error) renameOpen = false;
			}}
		>
			<Input aria-label="Project name" bind:value={projectName} />
			<Dialog.Footer class="mt-4"
				><Button type="submit" disabled={!projectName.trim()}>Save name</Button></Dialog.Footer
			>
		</form></Dialog.Content
	></Dialog.Root
>
