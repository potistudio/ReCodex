import { readFileSync } from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";

// `package.json` is the single source of the app version. Tauri and the UI read it from there, but Cargo cannot,
// so this checks that `desktop/Cargo.toml` still matches.

const read = (path) => readFileSync(fileURLToPath(new URL(`../${path}`, import.meta.url)), "utf8");

const expected = JSON.parse(read("package.json")).version;

/** Reads `version` from the `[package]` table of a Cargo manifest. */
function cargoVersion(manifest) {
	let inPackage = false;
	for (const line of manifest.split(/\r?\n/)) {
		const table = line.match(/^\[(.+)\]\s*$/);
		if (table) inPackage = table[1] === "package";
		else if (inPackage) {
			const match = line.match(/^version\s*=\s*"([^"]+)"/);
			if (match) return match[1];
		}
	}
	return undefined;
}

const actual = cargoVersion(read("desktop/Cargo.toml"));
if (actual !== expected) {
	console.error(
		`Version mismatch: package.json is ${expected}, but desktop/Cargo.toml is ${actual ?? "missing a version"}. ` +
			"Set the same version in both.",
	);
	process.exit(1);
}
