import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const pkgPath = join(root, "packages/proguard-plugin/package.json");
const gradlePropsPath = join(
	root,
	"packages/proguard-plugin/gradle.properties",
);

const pkg = JSON.parse(readFileSync(pkgPath, "utf8")) as { version: string };
writeFileSync(gradlePropsPath, `version=${pkg.version}\n`);
