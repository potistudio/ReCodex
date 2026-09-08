import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);
const codexRequire = createRequire(
	require.resolve("@openai/codex/package.json"),
);
const architecture = { x64: "x86_64", arm64: "aarch64" }[process.arch];
const target = {
	win32: `${architecture}-pc-windows-msvc`,
	darwin: `${architecture}-apple-darwin`,
	linux: `${architecture}-unknown-linux-musl`,
}[process.platform];

let executable = process.env.RECODEX_CODEX_PATH;
if (!executable) {
	if (!architecture || !target)
		throw new Error("Set RECODEX_CODEX_PATH for this platform.");
	const platformPackage = codexRequire.resolve(
		`@openai/codex-${process.platform}-${process.arch}/package.json`,
	);
	executable = path.join(
		path.dirname(platformPackage),
		"vendor",
		target,
		"bin",
		process.platform === "win32" ? "codex.exe" : "codex",
	);
}
if (!existsSync(executable))
	throw new Error(
		"Codex executable is missing. Run pnpm install or set RECODEX_CODEX_PATH.",
	);

const [command, ...args] = process.argv.slice(2);
const child =
	command === "tauri"
		? spawn(
				process.execPath,
				[require.resolve("@tauri-apps/cli/tauri.js"), ...args],
				{
					stdio: "inherit",
					env: { ...process.env, RECODEX_CODEX_PATH: executable },
					windowsHide: true,
				},
			)
		: spawn(command, args, {
				stdio: "inherit",
				env: { ...process.env, RECODEX_CODEX_PATH: executable },
				windowsHide: true,
			});
child.on("error", (error) => {
	console.error(error.message);
	process.exitCode = 1;
});
child.on("exit", (code) => {
	process.exitCode = code ?? 1;
});
for (const signal of ["SIGINT", "SIGTERM"])
	process.on(signal, () => child.kill(signal));
