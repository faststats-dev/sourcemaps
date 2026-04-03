import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { getChangelogSectionForVersion } from "./parse-changelog-section.js";

const scriptDir = dirname(fileURLToPath(import.meta.url));

const GRADLE_PLUGIN_ID = "dev.faststats.proguard-mappings-upload";

if (!process.env.REPOSITORY_TOKEN) {
	console.log("Skipping Gradle plugin publish (REPOSITORY_TOKEN unset)");
	process.exit(0);
}

const pluginDir = join(scriptDir, "..", "packages", "proguard-plugin");
const gradlew = join(pluginDir, "gradlew");

const result = spawnSync(
	gradlew,
	["publishPluginMavenPublicationToMavenRepository"],
	{
		cwd: pluginDir,
		stdio: "inherit",
		env: process.env,
		shell: false,
	},
);

if (result.status !== 0) {
	process.exit(result.status ?? 1);
}

const pkg = JSON.parse(
	readFileSync(join(pluginDir, "package.json"), "utf8"),
) as { version: string };

const tag = `${GRADLE_PLUGIN_ID}@${pkg.version}`;
const title = `${GRADLE_PLUGIN_ID} ${pkg.version}`;

const changelogPath = join(pluginDir, "CHANGELOG.md");
const changelogBody = getChangelogSectionForVersion(changelogPath, pkg.version);

if (!changelogBody) {
	console.error(
		`No ## ${pkg.version} section in ${changelogPath}. Run changeset version so Changesets updates the changelog.`,
	);
	process.exit(1);
}

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

const notesPath = join(
	tmpdir(),
	`gh-release-notes-${randomBytes(8).toString("hex")}.md`,
);
writeFileSync(notesPath, changelogBody, "utf8");

const createArgs = [
	"release",
	"create",
	tag,
	"--title",
	title,
	"--notes-file",
	notesPath,
];
if (process.env.GITHUB_SHA) {
	createArgs.push("--target", process.env.GITHUB_SHA);
}

try {
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
} finally {
	try {
		unlinkSync(notesPath);
	} catch {
		// ignore
	}
}

process.exit(0);
