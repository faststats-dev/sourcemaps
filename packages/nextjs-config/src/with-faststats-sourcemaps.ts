import { isAbsolute, join } from "node:path";
import type { BundlerPluginOptions } from "@faststats/sourcemap-uploader-plugin";
import { uploadSourcemapsFromDirectory } from "@faststats/sourcemap-uploader-plugin";
import createSourcemapsWebpackPlugin from "@faststats/sourcemap-uploader-plugin/webpack";

export type WithFaststatsSourcemapsOptions = BundlerPluginOptions & {
	useWebpackPlugin?: boolean | "auto";
	useRunAfterProductionCompile?: boolean | "auto";
};

type CompileParams = { distDir: string; projectDir: string };

type WebpackConfigLike = {
	plugins?: unknown[];
	[key: string]: unknown;
};

type WebpackContextLike = {
	dev: boolean;
	[key: string]: unknown;
};

export type NextConfigLike = {
	webpack?: (
		config: WebpackConfigLike,
		context: WebpackContextLike,
	) => WebpackConfigLike;
	compiler?: {
		runAfterProductionCompile?: (params: CompileParams) => void | Promise<void>;
		[key: string]: unknown;
	};
	[key: string]: unknown;
};

type NextConfigInternal = {
	webpack?: (
		config: WebpackConfigLike,
		context: WebpackContextLike,
	) => WebpackConfigLike;
	compiler?: {
		runAfterProductionCompile?: (params: CompileParams) => void | Promise<void>;
		[key: string]: unknown;
	};
};

const PLUGIN_OPTION_KEYS: ReadonlySet<string> = new Set([
	"enabled",
	"endpoint",
	"authToken",
	"buildId",
	"maxUploadBodyBytes",
	"failOnError",
	"deleteAfterUpload",
	"globalKey",
	"fetchImpl",
	"onUploadSuccess",
	"onUploadError",
	"sourcemapScanSkipDirectoryNames",
	"sourcemapScanRoots",
	"debug",
	"useWebpackPlugin",
	"useRunAfterProductionCompile",
]);

function isPluginOptionsOnly(value: object): boolean {
	const keys = Object.keys(value);
	if (keys.length === 0) {
		return true;
	}
	return keys.every((key) => PLUGIN_OPTION_KEYS.has(key));
}

function resolvePluginEnabled(
	options: WithFaststatsSourcemapsOptions,
): boolean {
	const enabled = options.enabled;
	if (typeof enabled === "function") {
		return enabled(undefined);
	}
	return enabled ?? true;
}

function isLikelyTurbopackBuild(): boolean {
	const turbo = process.env.TURBOPACK;
	if (turbo === "1" || turbo === "auto") {
		return true;
	}
	return process.argv.some(
		(arg) => arg.includes("turbopack") || arg.includes("--turbo"),
	);
}

function resolveUseWebpackPlugin(
	options: WithFaststatsSourcemapsOptions,
): boolean {
	const value = options.useWebpackPlugin ?? "auto";
	if (value === true) {
		return true;
	}
	if (value === false) {
		return false;
	}
	return !isLikelyTurbopackBuild();
}

function resolveUseRunAfterProductionCompile(
	options: WithFaststatsSourcemapsOptions,
): boolean {
	const value = options.useRunAfterProductionCompile ?? "auto";
	if (value === true) {
		return true;
	}
	if (value === false) {
		return false;
	}
	return isLikelyTurbopackBuild();
}

function resolveDistDir(params: CompileParams): string {
	return isAbsolute(params.distDir)
		? params.distDir
		: join(params.projectDir, params.distDir);
}

function applyWithFaststatsSourcemaps<T>(
	nextConfig: T,
	pluginOptions: WithFaststatsSourcemapsOptions,
): T {
	if (!resolvePluginEnabled(pluginOptions)) {
		return nextConfig;
	}

	const useWebpackPlugin = resolveUseWebpackPlugin(pluginOptions);
	const useHook = resolveUseRunAfterProductionCompile(pluginOptions);

	const {
		useWebpackPlugin: _omitWebpack,
		useRunAfterProductionCompile: _omitHook,
		...bundlerOptions
	} = pluginOptions;

	const internal = nextConfig as unknown as NextConfigInternal;
	const previousWebpack = internal.webpack;
	const previousRunAfterProductionCompile =
		internal.compiler?.runAfterProductionCompile;

	const chainWebpack = previousWebpack
		? (previousWebpack as (
				config: WebpackConfigLike,
				context: WebpackContextLike,
			) => WebpackConfigLike)
		: undefined;

	return {
		...nextConfig,
		webpack(config: WebpackConfigLike, context: WebpackContextLike) {
			const resolved = chainWebpack ? chainWebpack(config, context) : config;
			if (
				useWebpackPlugin &&
				!context.dev &&
				process.env.NODE_ENV === "production"
			) {
				resolved.plugins ??= [];
				resolved.plugins.push(createSourcemapsWebpackPlugin(bundlerOptions));
			}
			return resolved;
		},
		compiler: {
			...(internal.compiler ?? {}),
			runAfterProductionCompile: async (params: CompileParams) => {
				if (typeof previousRunAfterProductionCompile === "function") {
					await previousRunAfterProductionCompile(params);
				}
				if (!useHook || process.env.NODE_ENV !== "production") {
					return;
				}
				await uploadSourcemapsFromDirectory(resolveDistDir(params), {
					...bundlerOptions,
					sourcemapScanSkipDirectoryNames:
						bundlerOptions.sourcemapScanSkipDirectoryNames !== undefined
							? bundlerOptions.sourcemapScanSkipDirectoryNames
							: ["cache"],
					sourcemapScanRoots:
						bundlerOptions.sourcemapScanRoots !== undefined
							? bundlerOptions.sourcemapScanRoots
							: ["static", "server"],
				});
			},
		},
	} as T;
}

export function withFaststatsSourcemaps(
	pluginOptions: WithFaststatsSourcemapsOptions,
): <T>(nextConfig: T) => T;
export function withFaststatsSourcemaps<T>(
	nextConfig: T,
	pluginOptions?: WithFaststatsSourcemapsOptions,
): T;
export function withFaststatsSourcemaps(
	nextConfigOrPluginOptions: unknown,
	maybePluginOptions?: WithFaststatsSourcemapsOptions,
): unknown {
	if (maybePluginOptions !== undefined) {
		return applyWithFaststatsSourcemaps(
			nextConfigOrPluginOptions,
			maybePluginOptions,
		);
	}
	if (isPluginOptionsOnly(nextConfigOrPluginOptions as object)) {
		const pluginOptions =
			nextConfigOrPluginOptions as WithFaststatsSourcemapsOptions;
		return <T>(nextConfig: T) =>
			applyWithFaststatsSourcemaps(nextConfig, pluginOptions);
	}
	return applyWithFaststatsSourcemaps(nextConfigOrPluginOptions, {});
}
