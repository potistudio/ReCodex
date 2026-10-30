<script lang="ts">
import Icon from "@iconify/svelte";
import RefreshCwIconData from "@iconify-icons/fluent/arrow-sync-24-regular";
import ChevronLeftIconData from "@iconify-icons/fluent/chevron-left-24-regular";
import CircleIconData from "@iconify-icons/fluent/circle-24-filled";
import FileCodeIconData from "@iconify-icons/fluent/code-24-regular";
import XIconData from "@iconify-icons/fluent/dismiss-24-regular";
import FileIconData from "@iconify-icons/fluent/document-24-regular";
import FolderIconData from "@iconify-icons/fluent/folder-24-regular";
import SaveIconData from "@iconify-icons/fluent/save-24-regular";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { Button } from "$lib/components/ui/button";
import { reveal } from "$lib/motion";
import type { FileEntry, Project } from "$lib/types";

let {
	project,
	hostId = "local",
	revision,
	onclose,
	ondirty,
}: {
	project: Project;
	hostId?: string;
	revision: number;
	onclose: () => void;
	ondirty: (dirty: boolean) => void;
} = $props();
let entries = $state<FileEntry[]>([]);
let directory = $state("");
let file = $state("");
let content = $state("");
let original = $state("");
let error = $state("");
let loading = $state(false);
let saving = $state(false);
let saved = $state(false);
const dirty = $derived(content !== original);
$effect(() => ondirty(dirty));
$effect(() => {
	revision;
	void loadDirectory(directory);
});
async function loadDirectory(path: string) {
	loading = true;
	try {
		entries = await invoke<FileEntry[]>("files_list", {
			hostId,
			root: project.path,
			path,
		});
		directory = path;
	} catch (e) {
		error = String(e);
	} finally {
		loading = false;
	}
}
async function discard() {
	return (
		!dirty ||
		(await confirm("Discard unsaved changes to this file?", {
			title: "Unsaved changes",
			kind: "warning",
		}))
	);
}
async function openFile(path: string) {
	if (!(await discard())) return;
	error = "";
	loading = true;
	saved = false;
	try {
		const text = await invoke<string>("file_read", {
			hostId,
			root: project.path,
			path,
		});
		file = path;
		original = text;
		content = text;
	} catch (e) {
		error = String(e);
	} finally {
		loading = false;
	}
}
async function save() {
	if (!file || !dirty || saving) return;
	saving = true;
	error = "";
	const snapshot = content;
	try {
		await invoke("file_save", {
			hostId,
			root: project.path,
			path: file,
			content: snapshot,
			expected: original,
		});
		original = snapshot;
		saved = true;
	} catch (e) {
		error = String(e);
	} finally {
		saving = false;
	}
}
async function close() {
	if (await discard()) {
		ondirty(false);
		onclose();
	}
}
function keydown(event: KeyboardEvent) {
	if ((event.ctrlKey || event.metaKey) && event.key === "s") {
		event.preventDefault();
		void save();
	}
}
</script>

<svelte:window onkeydown={keydown} />
<aside class="file-panel" aria-label="Project files" transition:reveal|global={{ axis: "x" }}>
	<div class="panel-content">
		<header class="panel-header">
			<span><Icon icon={FileCodeIconData} width={16} />Files</span>
			<div>
				<Button
					variant="ghost"
					size="icon-sm"
					aria-label="Refresh files"
					onclick={() => loadDirectory(directory)}
					><Icon icon={RefreshCwIconData} width={15} /></Button
				><Button variant="ghost" size="icon-sm" aria-label="Close files" onclick={close}
					><Icon icon={XIconData} width={16} /></Button
				>
			</div>
		</header>
		<div class="file-browser">
			<div class="directory-heading">
				<Button
					variant="ghost"
					size="icon-xs"
					disabled={!directory || loading}
					aria-label="Parent directory"
					onclick={() => loadDirectory(directory.split("/").slice(0, -1).join("/"))}
					><Icon icon={ChevronLeftIconData} width={24} /></Button
				><span class="truncate" title={directory}>{directory || project.name}</span>
			</div>
			<div class="file-list">
				{#each entries as entry}
					<button
						type="button"
						class:active={file === entry.path}
						disabled={loading}
						onclick={() => (entry.isDirectory ? loadDirectory(entry.path) : openFile(entry.path))}
					>
						{#if entry.isDirectory}
							<Icon icon={FolderIconData} width={15} />
						{:else}
							<Icon icon={FileIconData} width={15} />
						{/if}
						<span class="truncate">{entry.name}</span>
					</button>
				{/each}
				{#if !entries.length}
					<p class="muted">
						{loading ? "Loading…" : "This folder is empty"}
					</p>
				{/if}
			</div>
		</div>
		{#if error}
			<div class="file-error" role="alert">{error}</div>
		{/if}
		{#if file}
			<div class="editor-toolbar">
				<span class="truncate" title={file}
					>{file}
					{#if dirty}
						<Icon icon={CircleIconData} width={7} />
					{/if}</span
				><Button
					variant="ghost"
					size="icon-xs"
					disabled={saving || loading}
					aria-label="Reload file from disk"
					onclick={() => openFile(file)}
					><Icon icon={RefreshCwIconData} width={24} /></Button
				><Button size="sm" disabled={!dirty || saving || loading} onclick={save}
					><Icon icon={SaveIconData} width={24} />{saving ? "Saving" : "Save"}</Button
				>
			</div>
			<textarea
				class="code-editor"
				aria-label={`Edit ${file}`}
				spellcheck="false"
				bind:value={content}
				oninput={() => (saved = false)}
			></textarea>
			<footer class="editor-status">
				<span>UTF-8</span><span>{content.split("\n").length} lines</span
				><span>{dirty ? "Unsaved" : saved ? "Saved" : "Up to date"}</span>
			</footer>
		{:else}
			<div class="editor-empty">
				<Icon icon={FileCodeIconData} width={28} />
				<p>A little room for your code.</p>
				<span>Open a file to view and edit it.</span>
			</div>
		{/if}
	</div>
</aside>
