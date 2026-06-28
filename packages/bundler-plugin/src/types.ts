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
