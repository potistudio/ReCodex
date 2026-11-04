import type { Model } from "./types";

/**
 * True when the selected model uses Codex and Codex still requires sign-in.
 * Pi and ReCodex harness models are usable without a ChatGPT account, so a
 * signed-out Codex account must not block them.
 */
export function requiresCodexAuth(model: Model | undefined, requiresAuth: boolean) {
	return requiresAuth && isCodexProvider(model?.provider);
}

/** Models and threads without a provider predate multi-provider support and belong to Codex. */
export function isCodexProvider(provider: string | undefined) {
	return (provider ?? "codex") === "codex";
}
