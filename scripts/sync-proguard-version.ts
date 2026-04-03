import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pkgPath = join(root, "packages/proguard-plugin/package.json");
const gradlePropsPath = join(
	root,
	"packages/proguard-plugin/gradle.properties",
);

const pkg = JSON.parse(readFileSync(pkgPath, "utf8")) as { version: string };
writeFileSync(gradlePropsPath, `version=${pkg.version}\n`);
