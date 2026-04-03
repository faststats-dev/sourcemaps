import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

if (!process.env.REPOSITORY_TOKEN) {
	console.log("Skipping Gradle plugin publish (REPOSITORY_TOKEN unset)");
	process.exit(0);
}

const pluginDir = join(import.meta.dir, "..", "packages", "proguard-plugin");
const gradlew = join(pluginDir, "gradlew");

const result = spawnSync(gradlew, ["publish"], {
	cwd: pluginDir,
	stdio: "inherit",
	env: process.env,
	shell: false,
});

if (result.status !== 0) {
	process.exit(result.status ?? 1);
}

const pkg = JSON.parse(
	readFileSync(join(pluginDir, "package.json"), "utf8"),
) as { name: string; version: string };
const tag = `${pkg.name}@${pkg.version}`;

if (process.env.GITHUB_ACTIONS !== "true") {
	process.exit(0);
}

const ghEnv = {
	...process.env,
	GH_TOKEN: process.env.GITHUB_TOKEN ?? process.env.GH_TOKEN ?? "",
};

if (!ghEnv.GH_TOKEN) {
	process.exit(0);
}

const view = spawnSync("gh", ["release", "view", tag], {
	env: ghEnv,
	stdio: "pipe",
});
if (view.status === 0) {
	process.exit(0);
}

const createArgs = [
	"release",
	"create",
	tag,
	"--title",
	`${pkg.name} ${pkg.version} (Gradle)`,
	"--notes",
	"Gradle plugin published to Maven. Two coordinates are normal: the `proguard-plugin` jar and the plugin marker for `dev.faststats.proguard-mappings-upload` (required for the `plugins { id(...) }` block).",
];
if (process.env.GITHUB_SHA) {
	createArgs.push("--target", process.env.GITHUB_SHA);
}

const created = spawnSync("gh", createArgs, {
	env: ghEnv,
	stdio: ["inherit", "inherit", "pipe"],
	encoding: "utf8",
});

if (created.status !== 0) {
	const err = created.stderr ?? "";
	if (
		err.includes("already_exists") ||
		err.toLowerCase().includes("already exists")
	) {
		process.exit(0);
	}
	if (err) {
		console.error(err);
	}
	process.exit(created.status ?? 1);
}

process.exit(0);
