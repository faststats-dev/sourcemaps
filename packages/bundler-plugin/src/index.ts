import { readdir, readFile, rm } from "node:fs/promises";
import { dirname, isAbsolute, join, relative } from "node:path";
import type {
	NormalizedOutputOptions,
	OutputAsset,
	OutputBundle,
	OutputOptions,
} from "rollup";
import { createUnplugin } from "unplugin";
import { getGitCommitHashSync } from "./utils/git";

const DEFAULT_ENDPOINT = "https://sourcemaps.faststats.dev/v0/upload";
const DEFAULT_MAX_UPLOAD_BODY_BYTES = 50 * 1024 * 1024;

type BundlerName =
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

export type UploadFile = {
	fileName: string;
	content: string;
};

export type UploadPayload = {
	type: "javascript";
	buildId: string;
	uploadedAt: string;
	files: UploadFile[];
};

export type BundlerPluginOptions = {
	enabled?: boolean | ((framework: BundlerName | undefined) => boolean);
	endpoint?: string;
	authToken?: string;
	buildId?: string;
	maxUploadBodyBytes?: number;
	failOnError?: boolean;
	deleteAfterUpload?: boolean;
	globalKey?: string;
	fetchImpl?: typeof fetch;
	onUploadSuccess?: (payload: UploadPayload) => void | Promise<void>;
	onUploadError?: (error: unknown) => void | Promise<void>;
};

const pluginName = "sourcemaps-bundler-plugin";

const resolveEnabled = (
	enabled: BundlerPluginOptions["enabled"],
	framework: BundlerName | undefined,
): boolean =>
	typeof enabled === "function" ? enabled(framework) : (enabled ?? true);

const createGlobalInjection = (globalKey: string, buildId: string): string =>
	`globalThis[${JSON.stringify(globalKey)}]={buildId:${JSON.stringify(buildId)}};`;

const collectUploadCandidates = (
	entries: Array<[string, unknown]>,
): UploadFile[] =>
	entries
		.filter(([fileName]) => fileName.endsWith(".map"))
		.flatMap(([fileName, source]) => {
			if (typeof source === "string") {
				return [{ fileName, content: source }];
			}

			if (
				source &&
				typeof source === "object" &&
				"toString" in source &&
				typeof source.toString === "function"
			) {
				return [{ fileName, content: source.toString() }];
			}

			return [];
		});

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

const postSourcemaps = async (
	options: BundlerPluginOptions,
	payload: UploadPayload,
): Promise<void> => {
	const fetchImpl = options.fetchImpl ?? fetch;
	const response = await fetchImpl(options.endpoint ?? DEFAULT_ENDPOINT, {
		method: "POST",
		headers: {
			"content-type": "application/json",
			...(options.authToken
				? { authorization: `Bearer ${options.authToken}` }
				: {}),
		},
		body: JSON.stringify(payload),
	});

	if (!response.ok) {
		throw new Error(`Sourcemap upload failed with status ${response.status}`);
	}
};

const payloadSizeBytes = (payload: UploadPayload): number =>
	Buffer.byteLength(JSON.stringify(payload), "utf8");

const createUploadBatches = (
	buildId: string,
	files: UploadFile[],
	maxUploadBodyBytes: number,
): UploadPayload[] => {
	if (!Number.isFinite(maxUploadBodyBytes) || maxUploadBodyBytes <= 0) {
		throw new Error("maxUploadBodyBytes must be a positive number");
	}

	const uploadedAt = new Date().toISOString();
	const batches: UploadPayload[] = [];
	let currentBatch: UploadFile[] = [];

	const toPayload = (batch: UploadFile[]): UploadPayload => ({
		type: "javascript",
		buildId,
		uploadedAt,
		files: batch,
	});

	const assertWithinLimit = (batch: UploadFile[], fileName: string) => {
		if (payloadSizeBytes(toPayload(batch)) > maxUploadBodyBytes) {
			throw new Error(
				`Sourcemap "${fileName}" exceeds maxUploadBodyBytes limit`,
			);
		}
	};

	for (const file of files) {
		const nextBatch = [...currentBatch, file];

		if (payloadSizeBytes(toPayload(nextBatch)) <= maxUploadBodyBytes) {
			currentBatch = nextBatch;
			continue;
		}

		if (currentBatch.length === 0) {
			assertWithinLimit([file], file.fileName);
		}

		batches.push(toPayload(currentBatch));
		currentBatch = [file];
		assertWithinLimit(currentBatch, file.fileName);
	}

	if (currentBatch.length > 0) {
		batches.push(toPayload(currentBatch));
	}

	return batches;
};

const handleUploadError = async (
	options: BundlerPluginOptions,
	error: unknown,
): Promise<void> => {
	await options.onUploadError?.(error);
	if (options.failOnError ?? true) {
		throw error;
	}
};

const deleteFiles = async (
	baseDir: string,
	fileNames: string[],
): Promise<void> => {
	await Promise.all(
		fileNames.map((fileName) =>
			rm(isAbsolute(fileName) ? fileName : join(baseDir, fileName), {
				force: true,
			}),
		),
	);
};

const scanDirectoryRecursively = async (rootDir: string): Promise<string[]> => {
	const entries = await readdir(rootDir, { withFileTypes: true });
	const nested = await Promise.all(
		entries.map(async (entry) => {
			const fullPath = join(rootDir, entry.name);
			if (entry.isDirectory()) {
				return scanDirectoryRecursively(fullPath);
			}
			return [fullPath];
		}),
	);

	return nested.flat();
};

const collectFromOutputDirectory = async (
	outputDir: string,
): Promise<UploadFile[]> => {
	const files = await scanDirectoryRecursively(outputDir);
	const sourcemapFiles = files.filter((filePath) => filePath.endsWith(".map"));
	return Promise.all(
		sourcemapFiles.map(async (filePath) => {
			const content = await readFile(filePath, "utf8");
			return {
				fileName: relative(outputDir, filePath),
				content,
			} satisfies UploadFile;
		}),
	);
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

const uploadAndMaybeDelete = async (
	options: BundlerPluginOptions,
	buildId: string,
	files: UploadFile[],
	baseDirForDeletion?: string,
): Promise<void> => {
	if (files.length === 0) {
		return;
	}

	const batches = createUploadBatches(
		buildId,
		files,
		options.maxUploadBodyBytes ?? DEFAULT_MAX_UPLOAD_BODY_BYTES,
	);
	for (const payload of batches) {
		await postSourcemaps(options, payload);
		await options.onUploadSuccess?.(payload);
	}

	if (options.deleteAfterUpload && baseDirForDeletion) {
		await deleteFiles(
			baseDirForDeletion,
			files.map((item) => item.fileName),
		);
	}
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

const rollupWriteBundle = (options: BundlerPluginOptions, buildId: string) =>
	async function (
		this: unknown,
		outputOptions: NormalizedOutputOptions,
		bundle: OutputBundle,
	): Promise<void> {
		try {
			const sourcemaps = collectFromBundle(bundle);
			if (sourcemaps.length === 0) return;

			const outputDir =
				outputOptions.dir ??
				(outputOptions.file ? dirname(outputOptions.file) : process.cwd());
			await uploadAndMaybeDelete(options, buildId, sourcemaps, outputDir);
		} catch (error) {
			await handleUploadError(options, error);
		}
	};

const rollupOutputOptions = (injection: string) =>
	function (this: unknown, outputOptions: OutputOptions): OutputOptions {
		return {
			...outputOptions,
			banner: createBanner(outputOptions.banner, injection),
		};
	};

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
	const BannerPlugin = compiler.webpack.BannerPlugin;
	new BannerPlugin({
		banner: createGlobalInjection(globalKey, buildId),
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
					if (sourcemaps.length === 0) {
						return;
					}

					const outputPath = compiler.options.output?.path;
					if (!outputPath) {
						return;
					}
					await uploadAndMaybeDelete(options, buildId, sourcemaps, outputPath);

					if (options.deleteAfterUpload && compilation.deleteAsset) {
						for (const item of sourcemaps) {
							compilation.deleteAsset(item.fileName);
						}
					}
				} catch (error) {
					await handleUploadError(options, error);
				}
			},
		);
	});
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

		const buildId =
			options.buildId ??
			getGitCommitHashSync() ??
			`random_${crypto.randomUUID()}`;
		const globalKey = options.globalKey ?? "__SOURCEMAPS_BUILD__";
		const injection = createGlobalInjection(globalKey, buildId);

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
								outputEntries.map(async (fileName) => {
									const content = await readFile(fileName, "utf8");
									return { fileName, content } satisfies UploadFile;
								}),
							);

							const outdir =
								build.initialOptions.outdir ??
								(build.initialOptions.outfile
									? dirname(build.initialOptions.outfile)
									: process.cwd());
							await uploadAndMaybeDelete(options, buildId, sourcemaps, outdir);
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

					const sourcemaps = await collectFromOutputDirectory(outputDir);
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
