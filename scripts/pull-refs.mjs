import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);

if (args.includes("--help") || args.includes("-h")) {
	console.log(`Usage: node scripts/pull-refs.mjs [directory]

Fast-forwards every Git repository directly under directory. The default is refs/.
Repositories are skipped when HEAD is detached or the current branch has no upstream.
Pull failures, including divergent branches, are reported per repository.
The script never creates merge commits.

Options:
  -h, --help  Show this message.`);
	process.exit(0);
}

const options = args.filter((arg) => arg.startsWith("-"));
const positional = args.filter((arg) => !arg.startsWith("-"));
if (options.length > 0) {
	console.error(`Unknown option: ${options[0]}. Run with --help for usage.`);
	process.exit(1);
}
if (positional.length > 1) {
	console.error("Expected at most one directory argument. Run with --help for usage.");
	process.exit(1);
}

const root = path.resolve(projectRoot, positional[0] ?? "refs");

function git(cwd, gitArgs) {
	return spawnSync("git", gitArgs, { cwd, encoding: "utf8", windowsHide: true });
}

const version = git(projectRoot, ["--version"]);
if (version.error || version.status !== 0) {
	console.error("Git is not available on PATH.");
	process.exit(1);
}

if (!existsSync(root) || !statSync(root).isDirectory()) {
	console.error(`Directory not found: ${root}`);
	process.exit(1);
}

const repositories = readdirSync(root, { withFileTypes: true })
	.filter((entry) => entry.isDirectory() && existsSync(path.join(root, entry.name, ".git")))
	.map((entry) => entry.name)
	.sort((left, right) => left.localeCompare(right));

if (repositories.length === 0) {
	console.log(`No Git repositories found in ${root}`);
	process.exit(0);
}

const results = [];
for (const [index, name] of repositories.entries()) {
	const directory = path.join(root, name);
	console.log(`\n[${index + 1}/${repositories.length}] ${name}`);

	const branch = git(directory, ["symbolic-ref", "--short", "-q", "HEAD"]);
	if (branch.status !== 0) {
		console.log("Skipped: detached HEAD");
		results.push({ name, status: "skipped", detail: "detached HEAD" });
		continue;
	}

	const upstream = git(directory, ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]);
	if (upstream.status !== 0) {
		console.log(`Skipped: ${branch.stdout.trim()} has no upstream branch`);
		results.push({ name, status: "skipped", detail: "no upstream branch" });
		continue;
	}

	console.log(`${branch.stdout.trim()} -> ${upstream.stdout.trim()}`);
	const headBefore = git(directory, ["rev-parse", "HEAD"]).stdout.trim();
	const pull = spawnSync("git", ["pull", "--ff-only", "--prune", "--quiet"], {
		cwd: directory,
		stdio: "inherit",
		windowsHide: true,
	});

	if (pull.status !== 0) {
		const reason = pull.signal ? `terminated by ${pull.signal}` : `git pull exited with code ${pull.status}`;
		results.push({ name, status: "failed", detail: reason });
		continue;
	}

	const headAfter = git(directory, ["rev-parse", "HEAD"]).stdout.trim();
	results.push({
		name,
		status: headBefore === headAfter ? "up-to-date" : "updated",
		detail: headBefore === headAfter ? "" : `${headBefore.slice(0, 7)}..${headAfter.slice(0, 7)}`,
	});
}

const statusWidth = Math.max(...results.map((result) => result.status.length));
const nameWidth = Math.max(...results.map((result) => result.name.length));

console.log(`\nSummary for ${root}`);
for (const result of results) {
	const line = `  ${result.status.padEnd(statusWidth)}  ${result.name.padEnd(nameWidth)}  ${result.detail}`;
	console.log(line.trimEnd());
}

process.exitCode = results.some((result) => result.status === "failed") ? 1 : 0;
