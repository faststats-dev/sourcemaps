import { readdir, readFile } from "node:fs/promises";
import { dirname, join, relative } from "node:path";
import type {
	NormalizedOutputOptions,
	OutputAsset,
	OutputBundle,
	OutputOptions,
} from "rollup";
import type {
	NativeBuildContext,
	RspackCompiler,
	UnpluginBuildContext,
	WebpackCompiler,
} from "unplugin";
import { createUnplugin } from "unplugin";
import type { BundlerName, BundlerPluginOptions, UploadFile } from "./types";
import { handleUploadError, uploadAndMaybeDelete } from "./upload";
import { getGitCommitHashSync } from "./utils/git";

export type {
	BundlerName,
	BundlerPluginOptions,
	UploadFile,
	UploadPayload,
} from "./types";

const PLUGIN_NAME = "sourcemaps-bundler-plugin";
const DEFAULT_GLOBAL_KEY = "__SOURCEMAPS_BUILD__";
const SKIP_NATIVE_UPLOAD = new Set<BundlerName>([
	"rollup",
	"vite",
	"rolldown",
	"unloader",
	"webpack",
	"rspack",
]);

type State = {
	options: BundlerPluginOptions;
	buildId: string;
	injection: string;
};

const dirFromOut = (outdir?: string, outfile?: string) =>
	outdir ?? (outfile ? dirname(outfile) : undefined);

const prepend = (head: string, tail?: string) =>
	tail ? `${head}\n${tail}` : head;

const mergeBanner = (
	existing: OutputOptions["banner"],
	injection: string,
): OutputOptions["banner"] => {
	if (typeof existing === "function") {
		return async (chunk) => prepend(injection, await existing(chunk));
	}
	if (typeof existing === "string") {
		return prepend(injection, existing);
	}
	return injection;
};

const assetSource = (source: unknown): string | undefined => {
	if (typeof source === "string") {
		return source;
	}
	if (
		source &&
		typeof source === "object" &&
		"toString" in source &&
		typeof source.toString === "function"
	) {
		return source.toString();
	}
	return undefined;
};

const mapsFromBundle = (bundle: OutputBundle): UploadFile[] =>
	Object.entries(bundle).flatMap(([fileName, entry]) => {
		if (!fileName.endsWith(".map") || entry.type !== "asset") {
			return [];
		}
		const content = assetSource((entry as OutputAsset).source);
		return content ? [{ fileName, content }] : [];
	});

const walk = async (rootDir: string): Promise<string[]> => {
	const entries = await readdir(rootDir, { withFileTypes: true });
	return (
		await Promise.all(
			entries.map((entry) => {
				const path = join(rootDir, entry.name);
				return entry.isDirectory() ? walk(path) : [path];
			}),
		)
	).flat();
};

const mapsFromDir = async (outputDir: string): Promise<UploadFile[]> =>
	Promise.all(
		(await walk(outputDir))
			.filter((path) => path.endsWith(".map"))
			.map(async (path) => ({
				fileName: relative(outputDir, path),
				content: await readFile(path, "utf8"),
			})),
	);

const nativeOutputDir = (native: NativeBuildContext): string | undefined => {
	switch (native.framework) {
		case "esbuild":
			return dirFromOut(
				native.build.initialOptions.outdir,
				native.build.initialOptions.outfile,
			);
		case "bun":
			return dirFromOut(
				native.build.config.outdir,
				native.build.config.outfile,
			);
		case "farm":
			return native.context.config.output.path;
	}
};

const guard = async (
	options: BundlerPluginOptions,
	run: () => Promise<void>,
): Promise<void> => {
	try {
		await run();
	} catch (error) {
		await handleUploadError(options, error);
	}
};

const uploadDir = async (state: State, outputDir: string) =>
	uploadAndMaybeDelete(
		state.options,
		state.buildId,
		await mapsFromDir(outputDir),
		outputDir,
	);

const afterEmitUpload = (
	compiler: {
		hooks: {
			afterEmit: {
				tapPromise: (name: string, callback: () => Promise<void>) => void;
			};
		};
		options: { output?: { path?: string } };
	},
	state: State,
): void => {
	compiler.hooks.afterEmit.tapPromise(PLUGIN_NAME, () =>
		guard(state.options, async () => {
			const outputDir = compiler.options.output?.path;
			if (outputDir) {
				await uploadDir(state, outputDir);
			}
		}),
	);
};

const bannerOptions = (injection: string) => ({
	banner: injection,
	raw: true,
	entryOnly: false,
});

const rollupHooks = (state: State) => ({
	outputOptions: (options: OutputOptions): OutputOptions => ({
		...options,
		banner: mergeBanner(options.banner, state.injection),
	}),
	writeBundle: async (options: NormalizedOutputOptions, bundle: OutputBundle) =>
		guard(state.options, async () => {
			const files = mapsFromBundle(bundle);
			if (files.length === 0) {
				return;
			}
			const outputDir =
				options.dir ?? (options.file ? dirname(options.file) : process.cwd());
			await uploadAndMaybeDelete(
				state.options,
				state.buildId,
				files,
				outputDir,
			);
		}),
});

const unpluginInstance = createUnplugin<BundlerPluginOptions>(
	(options, meta) => {
		const framework = meta.framework as BundlerName | undefined;
		const enabled =
			typeof options.enabled === "function"
				? options.enabled(framework)
				: (options.enabled ?? true);
		if (!enabled) {
			return { name: PLUGIN_NAME, enforce: "post" };
		}

		const buildId =
			options.buildId ??
			getGitCommitHashSync() ??
			`random_${crypto.randomUUID()}`;
		const state: State = {
			options,
			buildId,
			injection: `globalThis[${JSON.stringify(options.globalKey ?? DEFAULT_GLOBAL_KEY)}]={buildId:${JSON.stringify(buildId)}};`,
		};
		const hooks = rollupHooks(state);

		return {
			name: PLUGIN_NAME,
			enforce: "post",
			rollup: hooks,
			vite: hooks,
			rolldown: hooks as never,
			unloader: hooks,
			webpack(compiler: WebpackCompiler) {
				new compiler.webpack.BannerPlugin(bannerOptions(state.injection)).apply(
					compiler,
				);
				afterEmitUpload(compiler, state);
			},
			rspack(compiler: RspackCompiler) {
				new compiler.webpack.BannerPlugin(bannerOptions(state.injection)).apply(
					compiler,
				);
				afterEmitUpload(compiler, state);
			},
			esbuild: {
				config(buildOptions) {
					buildOptions.banner = {
						...(buildOptions.banner ?? {}),
						js: prepend(
							state.injection,
							typeof buildOptions.banner?.js === "string"
								? buildOptions.banner.js
								: undefined,
						),
					};
				},
			},
			buildEnd: async function (this: UnpluginBuildContext) {
				if (framework && SKIP_NATIVE_UPLOAD.has(framework)) {
					return;
				}
				await guard(state.options, async () => {
					const native = this.getNativeBuildContext?.();
					const outputDir = native && nativeOutputDir(native);
					if (outputDir) {
						await uploadDir(state, outputDir);
					}
				});
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
