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
import type { Project } from "$lib/types";
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
let followBottom = $state(true);
const activeWork = $derived(workingState(app.items, app.approvals.length > 0, app.thinkingLabel));
const filteredThreads = $derived(
	app.threads.filter((thread) => (thread.name || thread.preview).toLowerCase().includes(search.toLowerCase())),
);
const ready = $derived(app.connected && !!app.project && (!app.requiresAuth || !!app.account));
onMount(() => {
	dark = localStorage.getItem("recodex-theme") === "dark";
	void app.init();
	return () => app.dispose();
});
$effect(() => {
	document.documentElement.classList.toggle("dark", dark);
	localStorage.setItem("recodex-theme", dark ? "dark" : "light");
});
$effect(() => {
	app.items;
	app.activeTurn;
	app.approvals;
	if (followBottom && app.items.length)
		void tick().then(() => {
			if (chatScroll) chatScroll.scrollTop = chatScroll.scrollHeight;
		});
});
async function send() {
	const text = draft.trim();
	if (!text) return;
	draft = "";
	followBottom = true;
	if (!(await app.send(text))) draft = text;
	await tick();
	composer?.focus();
}
function inputKey(event: KeyboardEvent) {
	if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
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
		await app.chooseProject(project);
	}
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
		app.newChat();
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
						app.newChat();
					}}
					><span class="brand-mark"><Code2 size={21} strokeWidth={2.1} /></span>ReCodex</a
				><Button variant="ghost" size="icon" aria-label="Hide sidebar" onclick={() => (sidebar = false)}
					><PanelLeftClose /></Button
				>
			</div>
			<nav class="primary-nav" aria-label="Workspace">
				<button type="button" onclick={() => app.newChat()} disabled={!app.connected || app.loading}>
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
							followBottom = true;
							void app.resume(thread);
						}}
						title={thread.name || thread.preview}
					>
						<span class="truncate"
							>{thread.name ||
								thread.preview ||
								"Untitled chat"}</span
						>
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
				<button type="button" class="account-button" onclick={() => (settingsOpen = true)}>
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
				<div
					class="chat-scroll"
					bind:this={chatScroll}
					onscroll={() => {
						if (chatScroll)
							followBottom =
								chatScroll.scrollHeight -
									chatScroll.scrollTop -
									chatScroll.clientHeight <
								100;
					}}
				>
					{#if app.items.length === 0}
						<div class="welcome">
							<div class="welcome-eyebrow"><span></span>A little focus. A lot of possibility.</div>
							<h1>What will you build?</h1>
							<p>Bring an idea. Make it real.<br>Your code and conversations, in one quiet workspace.</p>
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
									<button
										type="button"
										onclick={() => {
							draft = suggestion.prompt;
											composer?.focus();
										}}
									>
										<suggestion.icon size={20} strokeWidth={1.5} />
										<strong>{suggestion.title}</strong><span>{suggestion.detail}</span>
										<ArrowUpRight size={14} class="suggestion-arrow" />
									</button>
								{/each}
							</div>
						</div>
					{:else}
						<div class="messages">
							{#each app.items as item (item.id)}
								<Message {item} />
							{/each}
							{#if app.activeTurn || app.sending}
								<div class="working">
									<span class="working-dot"></span>
									<span>{activeWork.label}</span>
									{#if activeWork.detail}
										<span class="working-detail">{activeWork.detail}</span>
									{/if}
								</div>
							{/if}
							{#each app.approvals as event (event.id)}
								<Approval
									{event}
									respond={(event, result) =>
									app.respond(event, result)}
									requestAlternative={(event) => app.requestAlternative(event)}
								/>
							{/each}
						</div>
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
