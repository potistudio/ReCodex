<script lang="ts">
import { invoke, isTauri } from "@tauri-apps/api/core";
import { onMount } from "svelte";
import { Button } from "$lib/components/ui/button";
import { Input } from "$lib/components/ui/input";
import type { HostCatalog, HostConnection } from "$lib/hosts";
import type { Project } from "$lib/types";

let {
	projects,
	onpaired,
	catalog,
	connections,
}: {
	projects: Project[];
	onpaired: (host: { id: string; name: string }) => Promise<void>;
	catalog: HostCatalog;
	connections: HostConnection[];
} = $props();
interface Status {
	id: string;
	name: string;
	address: string;
	sharedProjects?: string[];
	devices: { id: string; name: string }[];
}
let status = $state<Status | null>(null);
let name = $state("My PC");
let address = $state("0.0.0.0:43177");
let shared = $state<string[]>([]);
let invitation = $state("");
let remoteAddress = $state("");
let remoteInvitation = $state("");
let busy = $state(false);
let error = $state("");
async function refresh() {
	status = await invoke<Status | null>("host_status");
	if (status) {
		name = status.name;
		address = status.address;
		if (status.sharedProjects) shared = status.sharedProjects;
	}
}
onMount(() => {
	if (isTauri()) void run(refresh);
});
async function run(action: () => Promise<void>) {
	busy = true;
	error = "";
	try {
		await action();
	} catch (e) {
		error = String(e);
	} finally {
		busy = false;
	}
}
async function start() {
	await run(async () => {
		status = await invoke<Status>("host_start", { address, name, projects: shared });
		invitation = await invoke<string>("host_invite");
	});
}
async function stop() {
	await run(async () => {
		await invoke("host_stop");
		status = null;
		invitation = "";
	});
}
async function pair() {
	await run(async () => {
		const host = await invoke<{ id: string; name: string }>("host_pair", {
			address: remoteAddress.trim() || null,
			invitation: remoteInvitation.trim(),
		});
		remoteInvitation = "";
		await onpaired(host);
	});
}
</script>

<section class="host-settings" aria-label="LAN connections">
	<strong>LAN connections</strong>
	<p>Share selected projects on this PC, or connect to another PC running ReCodex.</p>
	{#if error}
		<p role="alert">{error}</p>
	{/if}
	{#if status}
		<div class="settings-row">
			<span><strong>{status.name}</strong><small>Hosting on {status.address}</small></span>
			<Button variant="outline" disabled={busy} onclick={stop}>Stop hosting</Button>
		</div>
		<p>Shared: {projects.filter((p) => shared.includes(p.path)).map((p) => p.name).join(", ")}</p>
		<Button
			variant="outline"
			size="sm"
			disabled={busy}
			onclick={() => run(async () => { invitation = await invoke<string>("host_invite"); await refresh(); })}
			>Create new invitation</Button
		>
		{#if invitation}
			<label for="host-invitation">Connection invitation</label>
			<textarea
				id="host-invitation"
				readonly
				value={invitation}
				onfocus={(e) => e.currentTarget.select()}
				rows="3"
			></textarea>
			<p>
				Copy this invitation to the other PC through a trusted channel. It expires in 10 minutes and can be used
				once. It grants control and approvals for the shared projects. Creating another invitation replaces this
				one.
			</p>
		{/if}
		<Button variant="ghost" size="sm" disabled={busy} onclick={() => run(refresh)}>Refresh devices</Button>
		{#each status.devices as device (device.id)}
			<div class="settings-row">
				<span>{device.name}</span
				><Button
					variant="outline"
					size="sm"
					disabled={busy}
					onclick={() => run(async () => { await invoke("host_revoke", { deviceId: device.id }); await refresh(); })}
					>Revoke</Button
				>
			</div>
		{/each}
	{:else}
		<label for="host-name">This PC's name</label>
		<Input id="host-name" bind:value={name} maxlength={80} />
		<label for="host-listen">Listen address</label>
		<Input id="host-listen" bind:value={address} placeholder="0.0.0.0:43177" />
		<fieldset disabled={busy}>
			<legend>Projects to share</legend>
			{#each projects as project (project.path)}
				<label class="shared-project"
					><input type="checkbox" bind:group={shared} value={project.path}><span>{project.name}</span></label
				>
			{:else}
				<p>Open a project on this PC first.</p>
			{/each}
		</fieldset>
		<Button variant="outline" disabled={busy || !isTauri() || !shared.length || !name.trim()} onclick={start}
			>Start hosting</Button
		>
	{/if}
	<section class="nearby-hosts" aria-label="Nearby PCs">
		<strong>Nearby PCs</strong>
		{#each catalog.discovered as host (host.id)}
			{@const known = connections.find((connection) => connection.id === host.id)}
			<div class="settings-row">
				<span
					><strong>{host.name}</strong
					><small
						>{known?.connected ? "Connected automatically" : known?.connecting ? "Connecting automatically…" : known ? "Reconnecting automatically…" : "Invitation required for first connection"}</small
					></span
				>
			</div>
		{:else}
			<p>Searching your LAN for ReCodex hosts…</p>
		{/each}
		{#each connections.filter((host) => !catalog.discovered.some((found) => found.id === host.id)) as host (host.id)}
			<div class="settings-row">
				<span
					><strong>{host.name}</strong
					><small
						>{host.connected ? "Connected" : host.connecting ? "Connecting automatically…" : "Waiting for this PC to return…"}</small
					></span
				>
			</div>
		{/each}
		{#if catalog.discoveryError}
			<p>Automatic discovery: {catalog.discoveryError}. You can use a manual address.</p>
		{/if}
		{#if catalog.hostingError}
			<p role="alert">Hosting: {catalog.hostingError}</p>
		{/if}
	</section>
	<form onsubmit={(event) => { event.preventDefault(); void pair(); }}>
		<strong>Connect to another PC</strong>
		<p>
			Paste an invitation once. ReCodex finds the matching PC automatically and remembers it for future
			connections.
		</p>
		<details>
			<summary>Manual address (optional)</summary>
			<label for="remote-address">Other PC's LAN address and port</label>
			<Input id="remote-address" bind:value={remoteAddress} placeholder="192.168.1.20:43177" />
		</details>
		<label for="remote-invitation">Invitation from the other PC</label>
		<textarea id="remote-invitation" bind:value={remoteInvitation} rows="3" spellcheck="false"></textarea>
		<Button type="submit" disabled={busy || !isTauri() || !remoteInvitation.trim()}
			>{busy ? "Connecting…" : "Connect to PC"}</Button
		>
	</form>
	<p>
		Connections use TLS and verify the certificate in the invitation. Registered PCs reconnect automatically after
		app restarts and address changes. Hosting resumes on launch until you choose Stop hosting. Keep ReCodex running
		on each host PC.
	</p>
</section>

<style>
.host-settings {
	display: grid;
	gap: 0.65rem;
	border-top: 1px solid var(--border);
	padding-top: 1rem;
}
p {
	font-size: 0.75rem;
	color: var(--muted-foreground);
	line-height: 1.5;
	overflow-wrap: anywhere;
}
p[role="alert"] {
	color: var(--destructive);
}
label,
legend {
	font-size: 0.8rem;
}
textarea {
	width: 100%;
	min-width: 0;
	border: 1px solid var(--border);
	border-radius: 0.4rem;
	padding: 0.5rem;
	font-size: 0.75rem;
	background: var(--background);
	color: var(--foreground);
	resize: vertical;
}
form {
	display: grid;
	gap: 0.65rem;
	border-top: 1px solid var(--border);
	padding-top: 1rem;
}
fieldset {
	display: grid;
	gap: 0.4rem;
	max-height: 9rem;
	overflow: auto;
}
.shared-project {
	display: flex;
	gap: 0.5rem;
	align-items: center;
}
</style>
