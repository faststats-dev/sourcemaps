import { DEBUG_ENV_KEY } from "./constants";
import type { JavaScriptSourcemapOptions } from "./types";

export const isSourcemapsDebug = (
	options: JavaScriptSourcemapOptions,
): boolean => options.debug === true || process.env[DEBUG_ENV_KEY] === "1";

export const debugLog = (
	options: JavaScriptSourcemapOptions,
	message: string,
	details?: Record<string, unknown>,
): void => {
	if (!isSourcemapsDebug(options)) {
		return;
	}

	const suffix =
		details && Object.keys(details).length > 0
			? ` ${JSON.stringify(details)}`
			: "";
	console.error(`[faststats:sourcemaps] ${message}${suffix}`);
};
