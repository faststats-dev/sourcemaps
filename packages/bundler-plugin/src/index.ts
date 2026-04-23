import { readFile } from "node:fs/promises";
import { dirname } from "node:path";
import {
	collectFromOutputDirectory,
	collectUploadCandidates,
	createBuildMetadataInjection,
	debugLog,
	handleUploadError,
	type JavaScriptSourcemapOptions,
	resolveBuildId,
	resolveGlobalKey,
	type UploadFile,
	uploadAndMaybeDelete,
} from "@faststats/sourcemap-uploader-core";
import type {
	NormalizedOutputOptions,
	OutputAsset,
	OutputBundle,
	OutputOptions,
} from "rollup";
import { createUnplugin } from "unplugin";

export type BundlerName =
	| "vite"
	| "rollup"
	| "rolldown"
	| "webpack"
	| "rspack"
	| "esbuild"
	| "farm"
	| "bun"
	| "unloader";

type WebpackLikeCompilation = {
	hooks: {
		processAssets: {
			tapPromise: (
				options: { name: string; stage: number },
				callback: (
					assets: Record<string, { source: () => unknown }>,
				) => Promise<void>,
			) => void;
		};
	};
	deleteAsset?: (fileName: string) => void;
};

type WebpackLikeCompiler = {
	webpack: {
		BannerPlugin: new (options: {
			banner: string | ((data: { hash?: string }) => string);
			raw?: boolean;
			entryOnly?: boolean;
		}) => { apply: (compiler: unknown) => void };
		Compilation: {
			PROCESS_ASSETS_STAGE_SUMMARIZE: number;
		};
	};
	options: {
		output?: {
			path?: string;
		};
	};
	hooks: {
		thisCompilation: {
			tap: (
				name: string,
				callback: (compilation: WebpackLikeCompilation) => void,
			) => void;
		};
	};
};

type NativeBuildContextLike = {
	getNativeBuildContext?: () => unknown;
};

export type {
	UploadFile,
	UploadPayload,
} from "@faststats/sourcemap-uploader-core";

export type BundlerPluginOptions = JavaScriptSourcemapOptions & {
	enabled?: boolean | ((framework: BundlerName | undefined) => boolean);
};

const pluginName = "sourcemaps-bundler-plugin";

const resolveEnabled = (
	enabled: BundlerPluginOptions["enabled"],
	framework: BundlerName | undefined,
): boolean =>
	typeof enabled === "function" ? enabled(framework) : (enabled ?? true);

const createBanner = (
	existingBanner: OutputOptions["banner"],
	injection: string,
): OutputOptions["banner"] => {
	if (typeof existingBanner === "function") {
		return async (chunk) => {
			const current = await existingBanner(chunk);
			return `${injection}\n${current}`;
		};
	}

	if (typeof existingBanner === "string") {
		return `${injection}\n${existingBanner}`;
	}

	return injection;
};

const isOutputAsset = (entry: OutputBundle[string]): entry is OutputAsset =>
	entry.type === "asset";

const collectFromBundle = (bundle: OutputBundle): UploadFile[] =>
	collectUploadCandidates(
		Object.entries(bundle).map(([fileName, entry]) => [
			fileName,
			isOutputAsset(entry) ? entry.source : null,
		]),
	);

const rollupWriteBundle =
	(options: BundlerPluginOptions, buildId: string) =>
	async (
		outputOptions: NormalizedOutputOptions,
		bundle: OutputBundle,
	): Promise<void> => {
		try {
			const sourcemaps = collectFromBundle(bundle);
			debugLog(options, "rollup writeBundle map outputs", {
				mapCount: sourcemaps.length,
			});
			if (sourcemaps.length === 0) {
				return;
			}

			const outputDir =
				outputOptions.dir ??
				(outputOptions.file ? dirname(outputOptions.file) : process.cwd());
			await uploadAndMaybeDelete(options, buildId, sourcemaps, outputDir);
		} catch (error) {
			await handleUploadError(options, error);
		}
	};

const rollupOutputOptions =
	(injection: string) =>
	(outputOptions: OutputOptions): OutputOptions => ({
		...outputOptions,
		banner: createBanner(outputOptions.banner, injection),
	});

const collectFromWebpackAssets = (
	assets: Record<string, { source: () => unknown }>,
): UploadFile[] =>
	collectUploadCandidates(
		Object.entries(assets)
			.filter(([assetName]) => assetName.endsWith(".map"))
			.map(([assetName, source]) => [assetName, source.source()]),
	);

const applyWebpackLikeHooks = (
	compiler: WebpackLikeCompiler,
	options: BundlerPluginOptions,
	globalKey: string,
	buildId: string,
): void => {
	new compiler.webpack.BannerPlugin({
		banner: createBuildMetadataInjection(globalKey, buildId),
		raw: true,
		entryOnly: false,
	}).apply(compiler);

	compiler.hooks.thisCompilation.tap(pluginName, (compilation) => {
		compilation.hooks.processAssets.tapPromise(
			{
				name: pluginName,
				stage: compiler.webpack.Compilation.PROCESS_ASSETS_STAGE_SUMMARIZE,
			},
			async (assets) => {
				try {
					const sourcemaps = collectFromWebpackAssets(assets);
					debugLog(options, "webpack processAssets", {
						mapAssetCount: sourcemaps.length,
						totalAssetCount: Object.keys(assets).length,
					});
					if (sourcemaps.length === 0) {
						return;
					}

					const outputPath = compiler.options.output?.path;
					if (!outputPath) {
						debugLog(options, "webpack processAssets missing output.path", {});
						return;
					}

					await uploadAndMaybeDelete(options, buildId, sourcemaps, outputPath);
					if (options.deleteAfterUpload && compilation.deleteAsset) {
						for (const sourcemap of sourcemaps) {
							compilation.deleteAsset(sourcemap.fileName);
						}
					}
				} catch (error) {
					await handleUploadError(options, error);
				}
			},
		);
	});
};

const getRecord = (value: unknown): Record<string, unknown> | undefined =>
	value && typeof value === "object"
		? (value as Record<string, unknown>)
		: undefined;

const getString = (value: unknown, key: string): string | undefined => {
	const record = getRecord(value);
	const field = record?.[key];
	return typeof field === "string" ? field : undefined;
};

const resolveNativeOutputDir = (nativeContext: unknown): string | undefined => {
	const native = getRecord(nativeContext);
	const framework = native?.framework;

	if (framework === "bun") {
		const buildConfig = getRecord(getRecord(native?.build)?.config);
		const outdir = getString(buildConfig, "outdir");
		if (outdir) {
			return outdir;
		}

		const outfile = getString(buildConfig, "outfile");
		return outfile ? dirname(outfile) : undefined;
	}

	if (framework === "farm") {
		const farmContext = getRecord(native?.context);
		const farmConfig = getRecord(farmContext?.config);
		const farmOutput = getRecord(farmConfig?.output);
		return getString(farmOutput, "path");
	}

	return undefined;
};

const unpluginInstance = createUnplugin<BundlerPluginOptions>(
	(options, meta) => {
		const framework = meta.framework as BundlerName | undefined;
		if (!resolveEnabled(options.enabled, framework)) {
			return {
				name: pluginName,
				enforce: "post",
			};
		}

		const buildId = resolveBuildId(options.buildId);
		const globalKey = resolveGlobalKey(options.globalKey);
		const injection = createBuildMetadataInjection(globalKey, buildId);

		return {
			name: pluginName,
			enforce: "post",
			rollup: {
				outputOptions: rollupOutputOptions(injection),
				writeBundle: rollupWriteBundle(options, buildId),
			},
			vite: {
				outputOptions: rollupOutputOptions(injection),
				writeBundle: rollupWriteBundle(options, buildId),
			},
			rolldown: {
				outputOptions: rollupOutputOptions(injection) as never,
				writeBundle: rollupWriteBundle(options, buildId) as never,
			},
			unloader: {
				outputOptions: rollupOutputOptions(injection),
				writeBundle: rollupWriteBundle(options, buildId),
			},
			webpack(compiler) {
				applyWebpackLikeHooks(
					compiler as unknown as WebpackLikeCompiler,
					options,
					globalKey,
					buildId,
				);
			},
			rspack(compiler) {
				applyWebpackLikeHooks(
					compiler as unknown as WebpackLikeCompiler,
					options,
					globalKey,
					buildId,
				);
			},
			esbuild: {
				setup(build) {
					const previousBanner = build.initialOptions.banner?.js;
					build.initialOptions.banner = {
						...(build.initialOptions.banner ?? {}),
						js: previousBanner ? `${injection}\n${previousBanner}` : injection,
					};

					if (!build.initialOptions.metafile) {
						build.initialOptions.metafile = true;
					}

					build.onEnd(async (result) => {
						try {
							const outputEntries = Object.keys(
								result.metafile?.outputs ?? {},
							).filter((name) => name.endsWith(".map"));
							const sourcemaps = await Promise.all(
								outputEntries.map(
									async (fileName) =>
										({
											fileName,
											content: await readFile(fileName, "utf8"),
										}) satisfies UploadFile,
								),
							);

							const outputDir =
								build.initialOptions.outdir ??
								(build.initialOptions.outfile
									? dirname(build.initialOptions.outfile)
									: process.cwd());
							await uploadAndMaybeDelete(
								options,
								buildId,
								sourcemaps,
								outputDir,
							);
						} catch (error) {
							await handleUploadError(options, error);
						}
					});
				},
			},
			buildEnd: async function (this: NativeBuildContextLike): Promise<void> {
				if (meta.framework !== "farm" && meta.framework !== "bun") {
					return;
				}

				try {
					const nativeContext = this.getNativeBuildContext?.();
					const outputDir = resolveNativeOutputDir(nativeContext);
					if (!outputDir) {
						return;
					}

					const sourcemaps = await collectFromOutputDirectory(
						outputDir,
						options,
					);
					if (sourcemaps.length === 0) {
						return;
					}

					await uploadAndMaybeDelete(options, buildId, sourcemaps, outputDir);
				} catch (error) {
					await handleUploadError(options, error);
				}
			},
		};
	},
);

export { uploadSourcemapsFromDirectory } from "@faststats/sourcemap-uploader-core";

export const sourcemapsPlugin = unpluginInstance;
export const vite = sourcemapsPlugin.vite;
export const rollup = sourcemapsPlugin.rollup;
export const rolldown = sourcemapsPlugin.rolldown;
export const webpack = sourcemapsPlugin.webpack;
export const rspack = sourcemapsPlugin.rspack;
export const esbuild = sourcemapsPlugin.esbuild;
export const unloader = sourcemapsPlugin.unloader;
export const farm = sourcemapsPlugin.farm;
export const bun = sourcemapsPlugin.bun;

type DefaultSourcemapsPlugin = typeof vite & typeof sourcemapsPlugin;
const defaultSourcemapsPlugin: DefaultSourcemapsPlugin = Object.assign(
	vite,
	sourcemapsPlugin,
);

export default defaultSourcemapsPlugin;
