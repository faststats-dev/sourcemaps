import type { Stats } from "node:fs";
import { readdir, readFile, realpath, rm, stat } from "node:fs/promises";
import { isAbsolute, join, relative } from "node:path";
import { resolveBuildId } from "./build";
import {
	BATCH_SIZE_ESTIMATE_MARGIN_CAP,
	DEFAULT_ENDPOINT,
	DEFAULT_MAX_UPLOAD_BODY_BYTES,
	MAP_READ_CONCURRENCY,
	MAP_SCAN_ENTRY_CONCURRENCY,
} from "./constants";
import { debugLog, isSourcemapsDebug } from "./debug";
import type {
	JavaScriptSourcemapOptions,
	UploadFile,
	UploadPayload,
} from "./types";

type ScanProgress = {
	dirsEntered: number;
	revisitSkipped: number;
	filesSeen: number;
	skippedByName: number;
};

export const collectUploadCandidates = (
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
	const filePieceBytes = files.map((file) =>
		Buffer.byteLength(
			JSON.stringify({ fileName: file.fileName, content: file.content }),
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
	const markerIndex = probe.indexOf(filesMarker);
	if (markerIndex === -1) {
		throw new Error("createUploadBatches: could not parse empty payload shape");
	}

	const head = probe.slice(0, markerIndex + filesMarker.length);
	const tail = probe.slice(markerIndex + filesMarker.length);
	const headTailBytes = Buffer.byteLength(head + tail, "utf8");

	const batches: UploadPayload[] = [];
	let fileIndex = 0;
	while (fileIndex < files.length) {
		const batchFiles: UploadFile[] = [];
		let batchBytes = headTailBytes;

		while (fileIndex < files.length) {
			const file = files[fileIndex] as UploadFile;
			const pieceBytes = filePieceBytes[fileIndex] as number;
			const separatorBytes = batchFiles.length > 0 ? 1 : 0;
			if (batchBytes + separatorBytes + pieceBytes <= budget) {
				batchFiles.push(file);
				batchBytes += separatorBytes + pieceBytes;
				fileIndex++;
				continue;
			}

			if (batchFiles.length > 0) {
				break;
			}

			const soloPayload: UploadPayload = {
				type: "javascript",
				buildId,
				uploadedAt,
				files: [file],
			};
			const soloBytes = Buffer.byteLength(JSON.stringify(soloPayload), "utf8");
			if (soloBytes > maxUploadBodyBytes) {
				throw new Error(
					`Sourcemap "${file.fileName}" exceeds maxUploadBodyBytes limit`,
				);
			}
			batches.push(soloPayload);
			fileIndex++;
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

export const postSourcemaps = async (
	options: JavaScriptSourcemapOptions,
	payload: UploadPayload,
): Promise<void> => {
	const body = JSON.stringify(payload);
	debugLog(options, "upload POST start", {
		filesInBatch: payload.files.length,
		bodyBytes: Buffer.byteLength(body, "utf8"),
		endpoint: options.endpoint ?? DEFAULT_ENDPOINT,
	});

	const fetchImpl = options.fetchImpl ?? fetch;
	const startedAt = Date.now();
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
		ms: Date.now() - startedAt,
		status: response.status,
		ok: response.ok,
	});

	if (!response.ok) {
		throw new Error(`Sourcemap upload failed with status ${response.status}`);
	}
};

export const handleUploadError = async (
	options: JavaScriptSourcemapOptions,
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
	options: JavaScriptSourcemapOptions,
	progress: ScanProgress,
): Promise<string[]> => {
	let canonicalPath: string;
	try {
		canonicalPath = await realpath(dirPath);
	} catch {
		return [];
	}

	if (visitedRealDirs.has(canonicalPath)) {
		progress.revisitSkipped++;
		if (
			isSourcemapsDebug(options) &&
			(progress.revisitSkipped <= 8 || progress.revisitSkipped % 500 === 0)
		) {
			debugLog(options, "scan skip revisiting path", {
				revisitSkipped: progress.revisitSkipped,
				canonical: canonicalPath,
			});
		}
		return [];
	}

	visitedRealDirs.add(canonicalPath);
	progress.dirsEntered++;
	if (
		isSourcemapsDebug(options) &&
		(progress.dirsEntered <= 16 || progress.dirsEntered % 250 === 0)
	) {
		debugLog(options, "scan entered directory", {
			dirsEntered: progress.dirsEntered,
			filesSeen: progress.filesSeen,
			canonical: canonicalPath,
		});
	}

	let dirents: import("node:fs").Dirent[];
	try {
		dirents = await readdir(dirPath, { withFileTypes: true });
	} catch {
		return [];
	}

	const mapPaths: string[] = [];
	for (
		let index = 0;
		index < dirents.length;
		index += MAP_SCAN_ENTRY_CONCURRENCY
	) {
		const chunk = dirents.slice(index, index + MAP_SCAN_ENTRY_CONCURRENCY);
		const nested = await Promise.all(
			chunk.map(async (entry) => {
				const name = entry.name;
				if (skipDirectoryNames.has(name)) {
					progress.skippedByName++;
					if (
						isSourcemapsDebug(options) &&
						(progress.skippedByName <= 12 || progress.skippedByName % 500 === 0)
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
					let stats: Stats;
					try {
						stats = await stat(fullPath);
					} catch {
						return [];
					}

					if (stats.isDirectory()) {
						return scanDirectoryForMapPaths(
							fullPath,
							visitedRealDirs,
							skipDirectoryNames,
							options,
							progress,
						);
					}

					if (stats.isFile() && name.endsWith(".map")) {
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

export const collectFromOutputDirectory = async (
	outputDir: string,
	options: JavaScriptSourcemapOptions,
): Promise<UploadFile[]> => {
	const skipDirectoryNames = new Set(
		options.sourcemapScanSkipDirectoryNames ?? [],
	);
	const roots =
		options.sourcemapScanRoots && options.sourcemapScanRoots.length > 0
			? options.sourcemapScanRoots.map((root) => join(outputDir, root))
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
	const scanStartedAt = Date.now();
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
		ms: Date.now() - scanStartedAt,
		dirsEntered: progress.dirsEntered,
		revisitSkipped: progress.revisitSkipped,
		filesSeen: progress.filesSeen,
		skippedByName: progress.skippedByName,
		mapPaths: sourcemapFiles.length,
	});

	const readStartedAt = Date.now();
	const result: UploadFile[] = [];
	for (
		let index = 0;
		index < sourcemapFiles.length;
		index += MAP_READ_CONCURRENCY
	) {
		const chunk = sourcemapFiles.slice(index, index + MAP_READ_CONCURRENCY);
		const files = await Promise.all(
			chunk.map(async (filePath) => {
				const content = await readFile(filePath, "utf8");
				return {
					fileName: relative(outputDir, filePath),
					content,
				} satisfies UploadFile;
			}),
		);
		result.push(...files);
	}

	debugLog(options, "collectFromOutputDirectory read maps done", {
		ms: Date.now() - readStartedAt,
		mapFilesRead: result.length,
	});

	return result;
};

export const uploadAndMaybeDelete = async (
	options: JavaScriptSourcemapOptions,
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

	const batchStartedAt = Date.now();
	const batches = createUploadBatches(
		buildId,
		files,
		options.maxUploadBodyBytes ?? DEFAULT_MAX_UPLOAD_BODY_BYTES,
	);
	debugLog(options, "batching done", {
		ms: Date.now() - batchStartedAt,
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
			files.map((file) => file.fileName),
		);
	}
};

export const uploadSourcemapsFromDirectory = async (
	outputDir: string,
	options: JavaScriptSourcemapOptions,
): Promise<void> => {
	debugLog(options, "uploadSourcemapsFromDirectory start", { outputDir });

	const buildIdStartedAt = Date.now();
	const buildId = resolveBuildId(options.buildId);
	debugLog(options, "uploadSourcemapsFromDirectory buildId", {
		buildId,
		resolvedMs: Date.now() - buildIdStartedAt,
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
};
