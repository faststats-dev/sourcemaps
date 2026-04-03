import { readFileSync } from "node:fs";

export function getChangelogSectionForVersion(
	changelogPath: string,
	version: string,
): string | null {
	let text: string;
	try {
		text = readFileSync(changelogPath, "utf8");
	} catch {
		return null;
	}
	const esc = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
	const header = new RegExp(`^## ${esc}\\s*$`, "m");
	const m = text.match(header);
	if (m === null || m.index === undefined) {
		return null;
	}
	const start = m.index + m[0].length;
	const rest = text.slice(start);
	const next = rest.search(/^## [0-9]/m);
	const body = (next === -1 ? rest : rest.slice(0, next)).trim();
	return body.length ? body : null;
}
