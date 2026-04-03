import { spawnSync } from "node:child_process";
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

process.exit(result.status ?? 1);
