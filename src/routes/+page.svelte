<script lang="ts">
import Icon from "@iconify/svelte";
import PlusIconData from "@iconify-icons/fluent/add-24-regular";
import LoaderCircleIconData from "@iconify-icons/fluent/arrow-sync-24-regular";
import ArrowUpIconData from "@iconify-icons/fluent/arrow-up-24-regular";
import ArrowUpRightIconData from "@iconify-icons/fluent/arrow-up-right-24-regular";
import ChevronDownIconData from "@iconify-icons/fluent/chevron-down-24-regular";
import Code2IconData from "@iconify-icons/fluent/code-24-regular";
import FileCodeIconData from "@iconify-icons/fluent/code-24-regular";
import SquarePenIconData from "@iconify-icons/fluent/compose-24-regular";
import Trash2IconData from "@iconify-icons/fluent/delete-24-regular";
import XIconData from "@iconify-icons/fluent/dismiss-24-regular";
import PencilIconData from "@iconify-icons/fluent/edit-24-regular";
import FolderIconData from "@iconify-icons/fluent/folder-24-regular";
import FolderPlusIconData from "@iconify-icons/fluent/folder-add-24-regular";
import GaugeIconData from "@iconify-icons/fluent/gauge-24-regular";
import MoreHorizontalIconData from "@iconify-icons/fluent/more-horizontal-24-regular";
import PanelLeftCloseIconData from "@iconify-icons/fluent/panel-left-contract-24-regular";
import PanelLeftOpenIconData from "@iconify-icons/fluent/panel-left-expand-24-regular";
import SearchIconData from "@iconify-icons/fluent/search-24-regular";
import Settings2IconData from "@iconify-icons/fluent/settings-24-regular";
import SquareIconData from "@iconify-icons/fluent/square-24-filled";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { platform } from "@tauri-apps/plugin-os";
import { Popover } from "bits-ui";
import { onMount, tick } from "svelte";
import { App } from "$lib/app.svelte";
import Approval from "$lib/components/Approval.svelte";
import FilePanel from "$lib/components/FilePanel.svelte";
import HostSettings from "$lib/components/HostSettings.svelte";
import Message from "$lib/components/Message.svelte";
import ModelEffortPicker from "$lib/components/ModelEffortPicker.svelte";
import ThemePicker from "$lib/components/ThemePicker.svelte";
import { Button } from "$lib/components/ui/button";
import * as Dialog from "$lib/components/ui/dialog";
import * as Dropdown from "$lib/components/ui/dropdown-menu";
import { Input } from "$lib/components/ui/input";
import { appear, reveal } from "$lib/motion";
import { applyTheme, readTheme } from "$lib/themes";
import type { Project, RateLimitSnapshot, RateLimitWindow, Thread } from "$lib/types";
import { formatResetTime, formatTokenCount, formatUsageWindow, remainingPercent } from "$lib/usage";
import { workingState } from "$lib/working";

const localApp = new App();
let hosts = $state([localApp]);
let app = $state(localApp);
const drafts: Record<string, string> = {};
async function selectHost(id: string) {
	if (id === app.hostId || !(await canLeaveFiles())) return;
	const next = hosts.find((host) => host.hostId === id);
	if (!next) return;
	filesOpen = false;
	fileDirty = false;
	drafts[app.hostId] = draft;
	draft = drafts[next.hostId] ?? "";
	search = "";
	scrollMode = "follow";
	app = next;
}
async function addHost(host: { id: string; name: string }) {
	const next = new App(host.id, host.name);
	hosts = [...hosts, next];
	await next.init();
	await selectHost(host.id);
}
async function forgetHost() {
	const previous = app;
	await selectHost("local");
	if (app === previous) return;
	previous.dispose();
	await invoke("host_forget", { hostId: previous.hostId });
	hosts = hosts.filter((host) => host !== previous);
}
let draft = $state("");
let search = $state("");
let searchOpen = $state(false);
let sidebar = $state(true);
let isMacClient = $state(false);
let filesOpen = $state(false);
let fileDirty = $state(false);
let settingsOpen = $state(false);
let renameOpen = $state(false);
let projectName = $state("");
let theme = $state(readTheme());
let chatScroll = $state<HTMLDivElement>();
let composer = $state<HTMLTextAreaElement>();
let composing = false;
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
const primaryRemaining = $derived(
	app.rateLimits?.rateLimits.primary ? remainingPercent(app.rateLimits.rateLimits.primary.usedPercent) : null,
);
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
	isMacClient = isTauri() && platform() === "macos";
	void localApp.init();

	return () => {
		if (labelTransitionTimer) clearTimeout(labelTransitionTimer);
		for (const host of hosts) host.dispose();
	};
});

$effect(() => {
	applyTheme(theme);
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
	if (!text || composing || !ready || app.busy) return;
	const wasFollowing = scrollMode === "follow";
	draft = "";
	const target = app;
	const sending = target.send(text);
	if (wasFollowing) {
		scrollMode = "free";
		await tick();
		centerLatestUserMessage();
	}
	if (!(await sending)) {
		if (app === target) draft = text;
		else drafts[target.hostId] = text;
	}
	if (app !== target) return;
	await tick();
	composer?.focus();
}
function inputKey(event: KeyboardEvent) {
	// WebKit may clear isComposing before the Enter that confirms an IME conversion.
	if (composing || event.isComposing || event.keyCode === 229) return;
	if (event.key === "Enter" && !event.shiftKey) {
		event.preventDefault();
		if (ready && !app.busy) void send();
	}
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
async function deleteThread(thread: Thread) {
	const name = thread.name || thread.preview || "Untitled chat";
	if (
		await confirm(`Permanently delete “${name}” and any chats it spawned? This cannot be undone.`, {
			title: "Delete chat",
			kind: "warning",
			okLabel: "Delete",
		})
	) {
		await app.deleteThread(thread);
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

<svelte:head>
	<title>ReCodex</title>
	<meta name="description" content="A quiet workspace for your next idea. Chat, code, and create with Codex.">
</svelte:head>

<svelte:window
	onkeydown={shortcuts}
	onbeforeunload={(event) => {
		if (fileDirty) {
			event.preventDefault();
			event.returnValue = "";
		}
	}}
/>

{#snippet rateLimitDetails()}
	<section class="rate-limit-panel" aria-label="Rate limits">
		<div>
			<strong>Rate limits</strong>
			<small>Remaining allowance from your signed-in Codex account.</small>
		</div>
		{#if rateLimitSnapshots.length}
			<div class="rate-limit-list">
				{#each rateLimitSnapshots as snapshot (snapshot.limitId ?? "default")}
					{#each rateLimitWindows(
							snapshot,
						) as window, index (`${snapshot.limitId ?? "default"}-${index}`)}
						<div class="rate-limit-row">
							<span
								><strong>{rateLimitName(snapshot)}</strong
								><small>{formatUsageWindow(window)}</small></span
							>
							<span class="rate-limit-value"
								><strong>{remainingPercent(window.usedPercent)}% remaining</strong>
								<small
									>{formatResetTime(window.resetsAt)
											? `Resets ${formatResetTime(window.resetsAt)}`
											: "Reset unavailable"}</small
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
{/snippet}

<header class="topbar" class:mac-window={isMacClient}>
	<div class="topbar-left">
		<Button
			variant="ghost"
			size="icon"
			aria-label={sidebar ? "Hide sidebar" : "Show sidebar"}
			onclick={() => (sidebar = !sidebar)}
		>
			{#if sidebar}
				<Icon icon={PanelLeftCloseIconData} width={24} />
			{:else}
				<Icon icon={PanelLeftOpenIconData} width={24} />
			{/if}
		</Button>
		<select
			class="host-selector"
			aria-label="Active host"
			value={app.hostId}
			onchange={(event) => { const select = event.currentTarget; void selectHost(select.value).then(() => { select.value = app.hostId; }); }}
		>
			{#each hosts as host (host.hostId)}
				<option value={host.hostId}>{host.hostName}</option>
			{/each}
		</select>
		<Button
			variant="ghost"
			size="icon"
			aria-label="Connections"
			title="LAN connections"
			onclick={() => (settingsOpen = true)}
			><Icon icon={Settings2IconData} width={18} /></Button
		>
	</div>

	<div class="topbar-right">
		<Popover.Root>
			<Popover.Trigger class="rate-limit-indicator" aria-label="Codex rate-limit allowance">
				<Icon icon={GaugeIconData} width={14} aria-hidden="true" />
				<span
					class="rate-limit-track"
					class:low={primaryRemaining !== null && primaryRemaining <= 20}
					aria-hidden="true"
				>
					{#if primaryRemaining !== null}
						<span style:width={`${primaryRemaining}%`}></span>
					{/if}
				</span>
				<strong>{primaryRemaining === null ? "—" : `${primaryRemaining}% left`}</strong>
			</Popover.Trigger>
			<Popover.Portal>
				<Popover.Content class="rate-limit-popover" align="end" sideOffset={8} aria-label="Rate limits">
					{@render rateLimitDetails()}
				</Popover.Content>
			</Popover.Portal>
		</Popover.Root>

		{#if app.project}
			<span class="project-breadcrumb"><Icon icon={FolderIconData} width={14} />{app.project.name}</span>
			<Button variant="ghost" size="sm" class={filesOpen ? "files-active" : ""} onclick={() => (filesOpen = true)}
				><Icon icon={FileCodeIconData} width={16} />Files</Button
			>
			<Dropdown.Root>
				<Dropdown.Trigger class="icon-button" aria-label="Project options" disabled={app.busy || app.remote}>
					<Icon icon={MoreHorizontalIconData} width={18} /></Dropdown.Trigger
				>
				<Dropdown.Content align="end">
					<Dropdown.Item
						onclick={() => {
							const project = app.project;
							if (!project) return;
							projectName = project.name;
							renameOpen = true;
						}}
						><Icon icon={PencilIconData} width={14} />Rename project</Dropdown.Item
					><Dropdown.Item onclick={relocateProject}>Change working directory</Dropdown.Item
					><Dropdown.Item onclick={removeProject}>Remove project</Dropdown.Item></Dropdown.Content
				></Dropdown.Root
			>
		{:else}
			<span class="topbar-hint">Your ideas, in progress.</span>
		{/if}
	</div>

	{#if isMacClient}
		<div class="topbar-drag-area" data-tauri-drag-region></div>
	{/if}
</header>

<div class="app-shell" class:sidebar-hidden={!sidebar}>
	{#if sidebar}
		<aside class="sidebar" transition:reveal={{ axis: "x" }}>
			<div class="panel-content">
				<nav class="primary-nav" aria-label="Workspace">
					<button type="button" onclick={newChat} disabled={!app.connected || app.loading}>
						<Icon icon={SquarePenIconData} width={18} /><span>New chat</span><kbd>Ctrl ⇧ O</kbd>
					</button>
					<button type="button" onclick={() => (searchOpen = !searchOpen)}>
						<Icon icon={SearchIconData} width={18} />
						<span>Search chats</span><kbd>Ctrl K</kbd>
					</button>
				</nav>
				{#if searchOpen}
					<div class="sidebar-search" transition:reveal>
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
						><Icon icon={PlusIconData} width={24} /></Button
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
							<Icon icon={FolderIconData} width={17} />
							<span class="truncate">{project.name}</span>
						</button>
					{/each}
				</div>
				<div class="section-heading recent-heading">
					<span>Recent chats</span>
					{#if app.loading}
						<Icon icon={LoaderCircleIconData} class="spin" width={13} />
					{/if}
				</div>
				<div class="thread-list">
					{#each filteredThreads as thread}
						<div class="thread-row">
							<button
								type="button"
								class:selected={app.thread?.id === thread.id}
								disabled={app.loading || !app.connected || app.deletingThreadId === thread.id}
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
									<Icon
										icon={LoaderCircleIconData}
										class="spin thread-running"
										width={14}
										aria-hidden="true"
									/>
								{/if}
							</button>
							<Button
								variant="ghost"
								size="icon-xs"
								class="thread-delete"
								aria-label={`Delete chat: ${thread.name || thread.preview || "Untitled chat"}`}
								disabled={!app.connected || app.loading || !!app.deletingThreadId || app.isThreadRunning(thread.id) || (app.thread?.id === thread.id && app.sending)}
								onclick={() => deleteThread(thread)}
								><Icon icon={Trash2IconData} width={24} /></Button
							>
						</div>
					{/each}
					{#if !filteredThreads.length}
						<div class="sidebar-empty">
							{search ? "No matching chats." : "Your conversations will appear here."}
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
							><strong>{app.account?.email?.split("@")[0] || "Your workspace"}</strong
							><small
								><span class="connection-dot" class:online={app.connected}></span>
								{app.connecting
									? "Connecting…"
									: app.connected
										? "Codex connected"
										: "Not connected"}</small
							></span
						><Icon icon={Settings2IconData} width={17} />
					</button>
				</div>
			</div>
		</aside>
	{/if}

	<main class="main-workspace">
		<div class="workspace-rim">
			<div class="workspace-body">
				<section class="chat-pane" aria-label="Chat">
					{#if !isTauri()}
						<div class="preview-banner">
							Browser preview · Open the desktop app to connect to Codex and your files.
						</div>
					{/if}
					{#if app.error}
						<div class="error-banner" role="alert" transition:reveal>
							<span>{app.error}</span
							><Button
								variant="ghost"
								size="icon-xs"
								aria-label="Dismiss error"
								onclick={() => (app.error = "")}
								><Icon icon={XIconData} width={24} /></Button
							>
						</div>
					{/if}
					<div class="chat-scroll-frame" class:chat-scroll-frame-empty={app.items.length === 0}>
						<div
							class="chat-scroll"
							bind:this={chatScroll}
							use:trackManualScroll
							onscroll={updateScrollMode}
						>
							{#if app.items.length > 0}
								<div class="messages">
									{#each app.items as item, itemIndex (item.renderKey ?? item.id)}
										<Message
											{item}
											tokenUsage={item.turnId ? app.tokenUsageForTurn(item.turnId) : null}
											generation={item.turnId && item.type === "agentMessage" && !app.items.slice(itemIndex + 1).some((next) => next.turnId === item.turnId && next.type === "agentMessage") ? app.generationForTurn(item.turnId) : null}
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
											hostName={app.hostName}
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
						{#each ["top", "bottom"] as edge}
							<div class="chat-edge-blur chat-edge-blur-{edge}" aria-hidden="true">
								{#each [1, 2, 3, 4, 5, 6] as layer}
									<span class="chat-edge-blur-layer chat-edge-blur-layer-{layer}"></span>
								{/each}
							</div>
						{/each}
						{#if scrollMode === "free"}
							<button
								class="scroll-to-latest"
								transition:appear
								type="button"
								aria-label="Scroll to latest"
								onclick={followLatest}
							>
								<Icon icon={ChevronDownIconData} width={18} />
							</button>
						{/if}
					</div>
					<div class="composer-area" class:composer-area-empty={app.items.length === 0}>
						{#if app.items.length === 0}
							<div class="welcome" in:appear>
								{#if !app.project}
									<Button
										variant="outline"
										size="lg"
										onclick={addProject}
										disabled={!isTauri() || app.remote || app.busy}
										><Icon icon={FolderPlusIconData} width={16} />Open a project<Icon
											icon={ArrowUpRightIconData}
											width={14}
										/></Button
									>
								{:else}
									<div class="workspace-pill">
										<Icon icon={FolderIconData} width={14} /><span>{app.project.name}</span
										><span class="pill-dot"></span><span>Ready to create</span>
									</div>
								{/if}
							</div>
						{/if}
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
								oncompositionstart={() => (composing = true)}
								oncompositionend={() => (composing = false)}
								aria-label="Message Codex"
								placeholder={app.project
									? "Ask Codex to build, explain, or fix anything…"
									: "What would you like to create?"}
								rows="2"
							></textarea>
							<div class="composer-toolbar">
								<div class="composer-context">
									<Icon icon={FolderIconData} width={14} />
									<span>{app.project?.name || "Select a project"}</span>
									<span class="toolbar-divider"></span>
									<ModelEffortPicker
										models={app.models}
										model={app.model}
										bind:effort={app.effort}
										disabled={app.busy}
										onModelChange={(model) => app.selectModel(model)}
									/>
								</div>
								{#if app.activeTurn}
									<Button size="icon" aria-label="Stop generation" onclick={() => app.stop()}
										><Icon icon={SquareIconData} width={13} /></Button
									>
								{:else}
									<Button
										type="submit"
										size="icon"
										class="send-button"
										aria-label="Send message"
										disabled={!draft.trim() || !ready || app.busy}
										>{#if app.sending}
											<Icon icon={LoaderCircleIconData} class="spin" width={24} />
										{:else}
											<Icon icon={ArrowUpIconData} width={18} />
										{/if}</Button
									>
								{/if}
							</div>
						</form>
						{#if isTauri() && !app.connected}
							<div class="connection-action">
								<Button
									variant="outline"
									size="sm"
									disabled={app.connecting}
									onclick={() => app.connect()}
									>{app.connecting ? "Connecting to Codex…" : "Connect to Codex"}</Button
								>
							</div>
						{:else if app.connected && app.requiresAuth && !app.account}
							<div class="connection-action">
								{#if app.remote}
									<span>Sign in to Codex on {app.hostName}, then reconnect.</span>
								{:else}
									<Button size="sm" onclick={() => app.login()}>Sign in with ChatGPT</Button>
								{/if}
							</div>
						{/if}
					</div>
				</section>
				{#if filesOpen && app.project}
					{#key `${app.hostId}:${app.project.path}`}
						<FilePanel
							hostId={app.hostId}
							project={app.project}
							revision={app.fileRevision}
							onclose={() => (filesOpen = false)}
							ondirty={(dirty) => (fileDirty = dirty)}
						/>
					{/key}
				{/if}
			</div>
		</div>
	</main>
</div>

<Dialog.Root bind:open={settingsOpen}
	><Dialog.Content class="settings-dialog"
		><Dialog.Header
			><Dialog.Title>Settings</Dialog.Title
			><Dialog.Description>A workspace that feels like yours.</Dialog.Description></Dialog.Header
		>
		<ThemePicker bind:value={theme} />
		<div class="settings-row">
			<span
				><strong>Codex connection</strong
				><small
					>{app.connected ? `Connected to ${app.hostName}` : `Not connected to ${app.hostName}`}</small
				></span
			><Button
				variant="outline"
				disabled={!isTauri() || app.connecting || (app.connected && app.busy)}
				onclick={() => app.connect()}
				>{app.connecting ? "Connecting…" : "Reconnect"}</Button
			>
		</div>
		<div class="settings-row">
			<span
				><strong>Project control</strong
				><small>Take over this project's changes, or release control for another device.</small></span
			>
			<Button variant="outline" disabled={!app.connected || !app.project} onclick={() => app.takeControl()}
				>Take control</Button
			>
			<Button variant="outline" disabled={!app.connected} onclick={() => app.releaseControl()}
				>Release control</Button
			>
		</div>
		{#if app.remote}
			<Button variant="outline" onclick={forgetHost}>Disconnect and forget {app.hostName}</Button>
		{/if}
		<HostSettings projects={localApp.projects} onpaired={addHost} />
		<div class="settings-row">
			<span
				><strong>Account</strong
				><small>{app.account?.email || app.account?.type || "Not signed in"}</small></span
			>
			{#if !app.account}
				<Button disabled={!app.connected || app.remote} onclick={() => app.login()}>Sign in</Button>
			{:else}
				<span class="account-plan">{app.account.planType || app.account.type}</span>
			{/if}
		</div>
		{@render rateLimitDetails()}
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
		<div class="settings-version">
			<Icon icon={Code2IconData} width={14} />ReCodex <span>0.0.1-alpha</span>
		</div></Dialog.Content
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
