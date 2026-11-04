import type { Model } from "./types";

/**
 * True when the selected model uses Codex and Codex still requires sign-in.
 * Pi models are usable without a ChatGPT account, so a signed-out Codex account
 * must not block them.
 */
export function requiresCodexAuth(model: Model | undefined, requiresAuth: boolean) {
	return requiresAuth && model?.provider !== "pi";
}
