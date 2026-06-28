import { rm } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import type { BundlerPluginOptions, UploadFile, UploadPayload } from "./types";

export const DEFAULT_ENDPOINT = "https://sourcemaps.faststats.dev/v0/upload";
export const DEFAULT_MAX_UPLOAD_BODY_BYTES = 50 * 1024 * 1024;

const payloadSizeBytes = (payload: UploadPayload): number =>
	Buffer.byteLength(JSON.stringify(payload), "utf8");

export const createUploadBatches = (
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

export const handleUploadError = async (
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

export const uploadAndMaybeDelete = async (
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
