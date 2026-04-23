import { DEFAULT_GLOBAL_KEY } from "./constants";
import { getGitCommitHashSync } from "./git";

export const resolveBuildId = (buildId?: string): string =>
	buildId ?? getGitCommitHashSync() ?? `random_${crypto.randomUUID()}`;

export const resolveGlobalKey = (globalKey?: string): string =>
	globalKey ?? DEFAULT_GLOBAL_KEY;

export const createBuildMetadataInjection = (
	globalKey: string,
	buildId: string,
): string =>
	`globalThis[${JSON.stringify(globalKey)}]={buildId:${JSON.stringify(buildId)}};`;
