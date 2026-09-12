<script lang="ts">
import { Moon, Sun } from "@lucide/svelte";
import { type Theme, themes } from "$lib/themes";

let { value = $bindable() }: { value: Theme } = $props();
</script>

<section class="theme-picker" aria-labelledby="appearance-heading">
	<div class="appearance-heading">
		<div>
			<h3 id="appearance-heading">Appearance</h3>
			<p>Choose a palette. Your workspace updates instantly.</p>
		</div>
		<span class="theme-count">{themes.length} themes</span>
	</div>
	<fieldset class="theme-grid">
		<legend class="sr-only">Color theme</legend>
		{#each themes as theme (theme.id)}
			<label class="theme-option" class:selected={value.id === theme.id}>
				<span
					class="theme-preview"
					style:--preview-background={theme.colors.background}
					style:--preview-foreground={theme.colors.foreground}
					style:--preview-sidebar={theme.colors.sidebar}
					style:--preview-card={theme.colors.card}
					style:--preview-border={theme.colors.border}
					style:--preview-primary={theme.colors.primary}
					aria-hidden="true"
				>
					<span class="theme-preview-sidebar">
						<span></span><span></span><span></span>
					</span>
					<span class="theme-preview-chat">
						<span class="theme-preview-title"></span>
						<span class="theme-preview-line"></span>
						<span class="theme-preview-line short"></span>
						<span class="theme-preview-composer"><span></span></span>
					</span>
				</span>
				<span class="theme-option-caption">
					<span class="theme-option-name">
						<strong>{theme.name}</strong>
						<small>
							{#if theme.mode === "dark"}
								<Moon size={10} aria-hidden="true" />
							{:else}
								<Sun size={10} aria-hidden="true" />
							{/if}
							{theme.mode === "dark" ? "Dark" : "Light"}
						</small>
					</span>
					<input
						type="radio"
						name="color-theme"
						value={theme.id}
						aria-label={theme.name}
						checked={value.id === theme.id}
						onchange={() => (value = theme)}
					>
				</span>
			</label>
		{/each}
	</fieldset>
	<p class="theme-selection" aria-live="polite">Current theme: <strong>{value.name}</strong></p>
</section>
