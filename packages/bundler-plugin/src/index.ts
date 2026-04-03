import type { Stats } from "node:fs";
import { readdir, readFile, realpath, rm, stat } from "node:fs/promises";
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
	sourcemapScanSkipDirectoryNames?: string[];
	sourcemapScanRoots?: string[];
	debug?: boolean;
};

const MAP_SCAN_ENTRY_CONCURRENCY = 64;
const MAP_READ_CONCURRENCY = 48;
const BATCH_SIZE_ESTIMATE_MARGIN_CAP = 2048;

const pluginName = "sourcemaps-bundler-plugin";

const DEBUG_ENV_KEY = "FASTSTATS_SOURCEMAPS_DEBUG";

const isSourcemapsDebug = (options: BundlerPluginOptions): boolean =>
	options.debug === true || process.env[DEBUG_ENV_KEY] === "1";

const debugLog = (
	options: BundlerPluginOptions,
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

type ScanProgress = {
	dirsEntered: number;
	revisitSkipped: number;
	filesSeen: number;
	skippedByName: number;
};

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
	const body = JSON.stringify(payload);
	debugLog(options, "upload POST start", {
		filesInBatch: payload.files.length,
		bodyBytes: Buffer.byteLength(body, "utf8"),
		endpoint: options.endpoint ?? DEFAULT_ENDPOINT,
	});
	const fetchImpl = options.fetchImpl ?? fetch;
	const t0 = Date.now();
	const response = await fetchImpl(options.endpoint ?? DEFAULT_ENDPOINT, {
		method: "POST",
		headers: {
			"content-type": "application/json",
			...(options.authToken
				? { authorization: `Bearer ${options.authToken}` }
				: {}),
		},
		body,
	});
	debugLog(options, "upload POST response", {
		ms: Date.now() - t0,
		status: response.status,
		ok: response.ok,
	});

	if (!response.ok) {
		throw new Error(`Sourcemap upload failed with status ${response.status}`);
	}
};

const createUploadBatches = (
	buildId: string,
	files: UploadFile[],
	maxUploadBodyBytes: number,
): UploadPayload[] => {
	if (!Number.isFinite(maxUploadBodyBytes) || maxUploadBodyBytes <= 0) {
		throw new Error("maxUploadBodyBytes must be a positive number");
	}

	const batchMargin = Math.min(
		BATCH_SIZE_ESTIMATE_MARGIN_CAP,
		Math.max(0, Math.floor(maxUploadBodyBytes * 0.05)),
	);
	const budget = Math.max(1, maxUploadBodyBytes - batchMargin);
	const uploadedAt = new Date().toISOString();
	const filePieceBytes = files.map((f) =>
		Buffer.byteLength(
			JSON.stringify({ fileName: f.fileName, content: f.content }),
			"utf8",
		),
	);

	const probe = JSON.stringify({
		type: "javascript" as const,
		buildId,
		uploadedAt,
		files: [],
	});
	const filesMarker = '"files":[';
	const mi = probe.indexOf(filesMarker);
	if (mi === -1) {
		throw new Error("createUploadBatches: could not parse empty payload shape");
	}
	const head = probe.slice(0, mi + filesMarker.length);
	const tail = probe.slice(mi + filesMarker.length);
	const headTailBytes = Buffer.byteLength(head + tail, "utf8");

	const batches: UploadPayload[] = [];
	let idx = 0;
	while (idx < files.length) {
		const batchFiles: UploadFile[] = [];
		let batchBytes = headTailBytes;

		while (idx < files.length) {
			const file = files[idx] as UploadFile;
			const pBytes = filePieceBytes[idx] as number;
			const extra = batchFiles.length > 0 ? 1 : 0;
			if (batchBytes + extra + pBytes <= budget) {
				batchFiles.push(file);
				batchBytes += extra + pBytes;
				idx++;
				continue;
			}
			if (batchFiles.length > 0) {
				break;
			}
			const solo: UploadPayload = {
				type: "javascript",
				buildId,
				uploadedAt,
				files: [file],
			};
			const soloBytes = Buffer.byteLength(JSON.stringify(solo), "utf8");
			if (soloBytes > maxUploadBodyBytes) {
				throw new Error(
					`Sourcemap "${file.fileName}" exceeds maxUploadBodyBytes limit`,
				);
			}
			batches.push(solo);
			idx++;
		}

		if (batchFiles.length > 0) {
			batches.push({
				type: "javascript",
				buildId,
				uploadedAt,
				files: batchFiles,
			});
		}
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

const scanDirectoryForMapPaths = async (
	dirPath: string,
	visitedRealDirs: Set<string>,
	skipDirectoryNames: ReadonlySet<string>,
	options: BundlerPluginOptions,
	progress: ScanProgress,
): Promise<string[]> => {
	let canonical: string;
	try {
		canonical = await realpath(dirPath);
	} catch {
		return [];
	}
	if (visitedRealDirs.has(canonical)) {
		progress.revisitSkipped++;
		if (
			isSourcemapsDebug(options) &&
			(progress.revisitSkipped <= 8 || progress.revisitSkipped % 500 === 0)
		) {
			debugLog(options, "scan skip revisiting path", {
				revisitSkipped: progress.revisitSkipped,
				canonical,
			});
		}
		return [];
	}
	visitedRealDirs.add(canonical);
	progress.dirsEntered++;
	if (
		isSourcemapsDebug(options) &&
		(progress.dirsEntered <= 16 || progress.dirsEntered % 250 === 0)
	) {
		debugLog(options, "scan entered directory", {
			dirsEntered: progress.dirsEntered,
			filesSeen: progress.filesSeen,
			canonical,
		});
	}

	let dirents: import("node:fs").Dirent[];
	try {
		dirents = await readdir(dirPath, { withFileTypes: true });
	} catch {
		return [];
	}

	const mapPaths: string[] = [];
	for (let c = 0; c < dirents.length; c += MAP_SCAN_ENTRY_CONCURRENCY) {
		const chunk = dirents.slice(c, c + MAP_SCAN_ENTRY_CONCURRENCY);
		const nested = await Promise.all(
			chunk.map(async (entry) => {
				const name = entry.name;
				if (skipDirectoryNames.has(name)) {
					progress.skippedByName++;
					if (
						isSourcemapsDebug(options) &&
						(progress.skippedByName <= 12 ||
							progress.skippedByName % 500 === 0)
					) {
						debugLog(options, "scan skip directory by name", {
							skippedByName: progress.skippedByName,
							name,
							parent: dirPath,
						});
					}
					return [] as string[];
				}
				const fullPath = join(dirPath, name);
				if (entry.isDirectory()) {
					return scanDirectoryForMapPaths(
						fullPath,
						visitedRealDirs,
						skipDirectoryNames,
						options,
						progress,
					);
				}
				if (entry.isFile() && name.endsWith(".map")) {
					progress.filesSeen++;
					if (
						isSourcemapsDebug(options) &&
						(progress.filesSeen <= 20 || progress.filesSeen % 2000 === 0)
					) {
						debugLog(options, "scan map file", {
							filesSeen: progress.filesSeen,
							path: fullPath,
						});
					}
					return [fullPath];
				}
				if (!entry.isFile() && !entry.isDirectory()) {
					let st: Stats;
					try {
						st = await stat(fullPath);
					} catch {
						return [];
					}
					if (st.isDirectory()) {
						return scanDirectoryForMapPaths(
							fullPath,
							visitedRealDirs,
							skipDirectoryNames,
							options,
							progress,
						);
					}
					if (st.isFile() && name.endsWith(".map")) {
						progress.filesSeen++;
						return [fullPath];
					}
				}
				return [];
			}),
		);
		for (const part of nested) {
			mapPaths.push(...part);
		}
	}
	return mapPaths;
};

const collectFromOutputDirectory = async (
	outputDir: string,
	options: BundlerPluginOptions,
): Promise<UploadFile[]> => {
	const skipDirectoryNames = new Set(
		options.sourcemapScanSkipDirectoryNames ?? [],
	);
	const roots =
		options.sourcemapScanRoots && options.sourcemapScanRoots.length > 0
			? options.sourcemapScanRoots.map((r) => join(outputDir, r))
			: [outputDir];
	debugLog(options, "collectFromOutputDirectory start", {
		outputDir,
		roots,
		skipDirectoryNames: [...skipDirectoryNames],
	});
	const progress: ScanProgress = {
		dirsEntered: 0,
		revisitSkipped: 0,
		filesSeen: 0,
		skippedByName: 0,
	};
	const scanT0 = Date.now();
	const visited = new Set<string>();
	const sourcemapFiles: string[] = [];
	for (const root of roots) {
		const found = await scanDirectoryForMapPaths(
			root,
			visited,
			skipDirectoryNames,
			options,
			progress,
		);
		sourcemapFiles.push(...found);
	}
	debugLog(options, "collectFromOutputDirectory scan done", {
		ms: Date.now() - scanT0,
		dirsEntered: progress.dirsEntered,
		revisitSkipped: progress.revisitSkipped,
		filesSeen: progress.filesSeen,
		skippedByName: progress.skippedByName,
		mapPaths: sourcemapFiles.length,
	});
	const readT0 = Date.now();
	const result: UploadFile[] = [];
	for (let i = 0; i < sourcemapFiles.length; i += MAP_READ_CONCURRENCY) {
		const slice = sourcemapFiles.slice(i, i + MAP_READ_CONCURRENCY);
		const part = await Promise.all(
			slice.map(async (filePath) => {
				const content = await readFile(filePath, "utf8");
				return {
					fileName: relative(outputDir, filePath),
					content,
				} satisfies UploadFile;
			}),
		);
		result.push(...part);
	}
	debugLog(options, "collectFromOutputDirectory read maps done", {
		ms: Date.now() - readT0,
		mapFilesRead: result.length,
	});
	return result;
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

	debugLog(options, "batching uploads", {
		mapFileCount: files.length,
		maxUploadBodyBytes:
			options.maxUploadBodyBytes ?? DEFAULT_MAX_UPLOAD_BODY_BYTES,
	});
	const batchT0 = Date.now();
	const batches = createUploadBatches(
		buildId,
		files,
		options.maxUploadBodyBytes ?? DEFAULT_MAX_UPLOAD_BODY_BYTES,
	);
	debugLog(options, "batching done", {
		ms: Date.now() - batchT0,
		batchCount: batches.length,
	});
	let batchIndex = 0;
	for (const payload of batches) {
		batchIndex++;
		debugLog(options, "upload batch", {
			index: batchIndex,
			of: batches.length,
			files: payload.files.length,
		});
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
			debugLog(options, "rollup writeBundle map outputs", {
				mapCount: sourcemaps.length,
			});
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

export async function uploadSourcemapsFromDirectory(
	outputDir: string,
	options: BundlerPluginOptions,
): Promise<void> {
	debugLog(options, "uploadSourcemapsFromDirectory start", { outputDir });
	const buildT0 = Date.now();
	const buildId =
		options.buildId ??
		getGitCommitHashSync() ??
		`random_${crypto.randomUUID()}`;
	debugLog(options, "uploadSourcemapsFromDirectory buildId", {
		buildId,
		resolvedMs: Date.now() - buildT0,
	});
	try {
		const sourcemaps = await collectFromOutputDirectory(outputDir, options);
		if (sourcemaps.length === 0) {
			debugLog(options, "uploadSourcemapsFromDirectory no map files", {
				outputDir,
			});
			return;
		}
		await uploadAndMaybeDelete(options, buildId, sourcemaps, outputDir);
		debugLog(options, "uploadSourcemapsFromDirectory complete", {
			outputDir,
			mapFiles: sourcemaps.length,
		});
	} catch (error) {
		debugLog(options, "uploadSourcemapsFromDirectory error", {
			message: error instanceof Error ? error.message : String(error),
		});
		await handleUploadError(options, error);
	}
}

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
