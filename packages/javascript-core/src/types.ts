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

export type JavaScriptSourcemapOptions = {
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
